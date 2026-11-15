use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    thread,
};

#[test]
fn fixture_streams_while_waiting_for_input() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_omniagent"))
        .args(["--fixture", "--stderr-flood", "--grandchild"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let drain =
        thread::spawn(move || std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink()));
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(
        output
            .next()
            .unwrap()
            .unwrap()
            .starts_with(r#"{"type":"grandchild","pid":"#)
    );
    assert_eq!(
        output.next().unwrap().unwrap(),
        r#"{"type":"ready","message":"Pronto ☕"}"#
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all("ação\n".as_bytes())
        .unwrap();
    assert_eq!(
        output.next().unwrap().unwrap(),
        r#"{"type":"input","bytes":7}"#
    );
    assert_eq!(
        output.next().unwrap().unwrap(),
        r#"{"type":"finished","code":0}"#
    );
    assert!(child.wait().unwrap().success());
    assert_eq!(drain.join().unwrap().unwrap(), 1_049_600);
}
