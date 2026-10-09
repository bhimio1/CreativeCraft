use crate::{AgentError, ChatEvent, Parser};
use creativecraft_platform::HostCommand;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

/// A turn in progress. Events arrive on [`Running::events`]; the last one is always
/// [`ChatEvent::Done`].
pub struct Running {
    child: Arc<Mutex<Child>>,
    pub events: Receiver<ChatEvent>,
}

impl Running {
    pub(crate) fn spawn(mut cmd: HostCommand, mut parser: Parser) -> Result<Running, AgentError> {
        // ask CLIs for plain, uncoloured, non-interactive output (set on the HostCommand, so the
        // variables reach the host process inside Flatpak too)
        cmd.env("NO_COLOR", "1").env("CI", "1").env("TERM", "dumb");
        let mut cmd = cmd.into_std();
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = channel();

        // stderr: kept (last ~4 KB) to explain failures
        let err_buf = Arc::new(Mutex::new(String::new()));
        let err_thread = stderr.map(|mut e| {
            let buf = err_buf.clone();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.read_to_string(&mut s);
                if let Ok(mut b) = buf.lock() {
                    let start = s.len().saturating_sub(4096);
                    let start = (start..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(s.len());
                    *b = s[start..].to_string();
                }
            })
        });

        let waiter = child.clone();
        std::thread::spawn(move || {
            let mut errored = false;
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    for ev in parser.line(&line) {
                        errored |= matches!(ev, ChatEvent::Error(_));
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                }
            }
            if let Some(t) = err_thread {
                let _ = t.join();
            }
            let status = waiter.lock().ok().and_then(|mut c| c.wait().ok());
            if let Some(st) = status
                && !st.success()
                && !errored
            {
                let tail = err_buf.lock().map(|b| b.trim().to_string()).unwrap_or_default();
                let msg = if tail.is_empty() { format!("the agent exited with {st}") } else { tail };
                let _ = tx.send(ChatEvent::Error(msg));
            }
            let _ = tx.send(ChatEvent::Done);
        });
        Ok(Running { child, events: rx })
    }

    /// Stop the turn (the backend keeps whatever it already saved of the session).
    pub fn cancel(&self) {
        if let Ok(mut c) = self.child.lock() {
            creativecraft_platform::stop(&mut c);
        }
    }
}
