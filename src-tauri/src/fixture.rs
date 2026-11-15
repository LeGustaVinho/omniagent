use std::{
    env,
    io::{self, BufRead, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const GRANDCHILD_LIFETIME: Duration = Duration::from_secs(60);

pub fn run_from_args() -> Option<i32> {
    let mut args = env::args_os().skip(1);
    match args.next()?.to_str()? {
        "--fixture-grandchild" => {
            thread::sleep(GRANDCHILD_LIFETIME);
            Some(0)
        }
        "--fixture" => {
            let mut options = Options::default();
            for arg in args {
                match arg.to_str() {
                    Some("--stderr-flood") => options.stderr_flood = true,
                    Some("--grandchild") => options.grandchild = true,
                    _ => return Some(2),
                }
            }
            match run(options) {
                Ok(code) => Some(code),
                Err(error) => {
                    eprintln!("Fixture failed: {error}");
                    Some(1)
                }
            }
        }
        _ => None,
    }
}

#[derive(Default)]
struct Options {
    stderr_flood: bool,
    grandchild: bool,
}

fn run(options: Options) -> io::Result<i32> {
    let grandchild = options.grandchild.then(spawn_grandchild).transpose()?;
    let noise = options.stderr_flood.then(|| thread::spawn(flood_stderr));
    let mut stdout = io::stdout().lock();
    if let Some(child) = &grandchild {
        writeln!(
            stdout,
            "{{\"type\":\"grandchild\",\"pid\":{}}}",
            child.0.id()
        )?;
    }
    // Split the UTF-8 bytes across writes to exercise decoders that consume chunks.
    let ready = "{\"type\":\"ready\",\"message\":\"Pronto ☕\"}\n";
    let split = ready.find('☕').expect("fixture marker is present") + 1;
    stdout.write_all(&ready.as_bytes()[..split])?;
    stdout.flush()?;
    thread::sleep(Duration::from_millis(25));
    stdout.write_all(&ready.as_bytes()[split..])?;
    stdout.flush()?;

    // Report the byte count only. The harness can compare exact bytes independently.
    let mut input = Vec::new();
    let received = io::stdin().lock().read_until(b'\n', &mut input)?;
    writeln!(stdout, "{{\"type\":\"input\",\"bytes\":{received}}}")?;
    stdout.flush()?;
    if let Some(noise) = noise {
        noise
            .join()
            .map_err(|_| io::Error::other("stderr writer panicked"))??;
    }
    let code = if received == 0 { 3 } else { 0 };
    writeln!(stdout, "{{\"type\":\"finished\",\"code\":{code}}}")?;
    stdout.flush()?;
    Ok(code)
}

fn flood_stderr() -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    let line = [b'x'; 1024];
    for _ in 0..1024 {
        stderr.write_all(&line)?;
        stderr.write_all(b"\n")?;
    }
    stderr.flush()
}

fn spawn_grandchild() -> io::Result<FixtureChild> {
    Command::new(env::current_exe()?)
        .arg("--fixture-grandchild")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(FixtureChild)
}

struct FixtureChild(Child);

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
