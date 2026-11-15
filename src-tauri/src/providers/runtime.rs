use std::{sync::mpsc, thread, time::Instant};

use anyhow::{Context, Result};

use crate::{process::TerminalOutcome, process_io::CapturedProcess};

use super::{
    AdapterUpdate, CliSession, ProviderEvent, ProviderEventData, TurnOutcome, WireMessage,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Interruption {
    PromptReturned(TurnOutcome),
    ProcessStopped(TerminalOutcome),
}

/// Settle the outstanding turn after requesting an interrupt. The watchdog
/// stays independent of synchronous pipe writes so a wedged child cannot
/// prevent the supervisor from closing the job.
pub fn interrupt_to_completion(
    process: &mut CapturedProcess,
    session: &mut dyn CliSession,
    mut publish: impl FnMut(ProviderEvent) -> Result<()>,
) -> Result<Interruption> {
    let interruption = session.interrupt()?;
    let deadline = Instant::now() + interruption.grace;
    let control = process.control.clone();
    let (settled, wait) = mpsc::channel();
    thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            if wait
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                control.cancel()?;
            }
            Ok::<_, std::io::Error>(())
        });

        let observed = (|| {
            write_frames(process, interruption.outbound)?;
            while Instant::now() < deadline {
                let frame = match process
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(frame) => frame?,
                    Err(_) => break,
                };
                let response = session.receive(frame)?;
                if let Some(outcome) = deliver_update(process, response, &mut publish)? {
                    return Ok::<_, anyhow::Error>(Some(outcome));
                }
            }
            Ok(None)
        })();
        if let Ok(Some(outcome)) = observed {
            let _ = settled.send(());
            watchdog.join().expect("interruption watchdog")?;
            return Ok(Interruption::PromptReturned(outcome));
        }

        let cancellation = process.control.cancel();
        let _ = settled.send(());
        let guarded = watchdog.join().expect("interruption watchdog");
        if let Err(error) = cancellation {
            guarded.context("watchdog could not stop the CLI job")?;
            return Err(error).context("could not stop the CLI job");
        }
        let exit = process.control.wait()?;
        deliver_update(process, session.process_finished(exit), &mut publish)?;
        observed?;
        Ok(Interruption::ProcessStopped(exit))
    })
}

fn deliver_update(
    process: &mut CapturedProcess,
    update: AdapterUpdate,
    publish: &mut impl FnMut(ProviderEvent) -> Result<()>,
) -> Result<Option<TurnOutcome>> {
    write_frames(process, update.outbound)?;
    let mut terminal = None;
    for event in update.events {
        if let ProviderEventData::TurnFinished { outcome } = event.data {
            terminal = Some(outcome);
        }
        publish(event)?;
    }
    Ok(terminal)
}

fn write_frames(process: &mut CapturedProcess, frames: Vec<WireMessage>) -> Result<()> {
    for request in frames {
        process.input.send(request.as_bytes())?;
    }
    Ok(())
}
