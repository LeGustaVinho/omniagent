use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use crate::process::{WindowsProcess, WindowsProcessControl};

const FRAME_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub struct OutputFrame {
    pub stream: Stream,
    pub data: String,
}

pub struct CapturedProcess {
    pub control: Arc<WindowsProcessControl>,
    pub input: ProcessInput,
    pub output: Receiver<io::Result<OutputFrame>>,
}

pub struct ProcessInput(File);

impl ProcessInput {
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }
}

pub fn capture(process: WindowsProcess) -> io::Result<CapturedProcess> {
    // Bound buffered events. The consumer drains while the process is running.
    let (sender, output) = mpsc::sync_channel(32);
    start_reader(process.stdout, Stream::Stdout, sender.clone())?;
    start_reader(process.stderr, Stream::Stderr, sender)?;
    Ok(CapturedProcess {
        control: Arc::new(process.control),
        input: ProcessInput(process.stdin),
        output,
    })
}

fn start_reader(
    pipe: File,
    stream: Stream,
    sender: SyncSender<io::Result<OutputFrame>>,
) -> io::Result<()> {
    let name = match stream {
        Stream::Stdout => "cli-stdout",
        Stream::Stderr => "cli-stderr",
    };
    thread::Builder::new().name(name.into()).spawn(move || {
        if let Err(error) = read_frames(pipe, stream, &sender) {
            let _ = sender.send(Err(error));
        }
    })?;
    Ok(())
}

fn read_frames(
    input: impl Read,
    stream: Stream,
    sender: &SyncSender<io::Result<OutputFrame>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(input);
    let mut frame = Vec::new();
    loop {
        frame.clear();
        let read = (&mut reader)
            .take((FRAME_LIMIT + 1) as u64)
            .read_until(b'\n', &mut frame)?;
        if read == 0 {
            return Ok(());
        }
        if read > FRAME_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{stream:?} frame exceeds {FRAME_LIMIT} bytes"),
            ));
        }
        let data = match stream {
            Stream::Stdout => std::str::from_utf8(&frame)
                .map_err(io::Error::other)?
                .to_owned(),
            Stream::Stderr => String::from_utf8_lossy(&frame).into_owned(),
        };
        if sender.send(Ok(OutputFrame { stream, data })).is_err() {
            return Ok(());
        }
    }
}
