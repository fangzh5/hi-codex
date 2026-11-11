//! Opt-in, bounded state-transition logging. No background thread or default I/O.
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy)]
pub enum Event {
    Render,
    Position,
    Measurement,
    NativeBand,
}

struct Log {
    path: PathBuf,
    last: [Option<u32>; 4],
}

impl Log {
    fn record(&mut self, event: Event, code: u32) -> std::io::Result<()> {
        let index = event as usize;
        if self.last[index] == Some(code) {
            return Ok(());
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let line = format!("{time} event={index} code={code}\n");
        // Remember even a failed attempt: an unwritable diagnostic path must
        // not trigger a file-open storm on every paint or timer tick.
        self.last[index] = Some(code);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.path)?;
        if file.metadata()?.len().saturating_add(line.len() as u64) > MAX_BYTES {
            file.set_len(0)?;
        }
        file.seek(SeekFrom::End(0))?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

pub fn record(event: Event, code: u32) {
    static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();
    let Some(log) = LOG.get_or_init(|| {
        std::env::var_os("HICODEX_DIAGNOSTIC").map(|path| {
            Mutex::new(Log {
                path: path.into(),
                last: [None; 4],
            })
        })
    }) else {
        return;
    };
    // A reentrant Windows callback must never wait for logging.
    if let Ok(mut log) = log.try_lock() {
        let _ = log.record(event, code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_suppresses_unchanged_state_and_caps_disk_use() {
        let path =
            std::env::temp_dir().join(format!("hicodex-diagnostic-{}.log", std::process::id()));
        let mut log = Log {
            path: path.clone(),
            last: [None; 4],
        };
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize]).unwrap();
        log.record(Event::Render, 0).unwrap();
        let length = std::fs::metadata(&path).unwrap().len();
        assert!(length < 100);
        for _ in 0..100 {
            log.record(Event::Render, 0).unwrap();
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
        for code in 1..3000 {
            log.record(Event::Render, code).unwrap();
        }
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_BYTES);
        std::fs::remove_file(path).unwrap();
    }
}
