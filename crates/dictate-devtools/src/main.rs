//! `devtool`: tiny helpers that repo shell scripts call where plain shell is
//! impractical. Replaces the inline `python3` snippets (Issue #3690).
//!
//! ```text
//! devtool free-port                       print a free 127.0.0.1 TCP port
//! devtool stale-uds PATH                  bind a Unix socket, close it, leave the inode
//! devtool plant-symlinks VICTIM DIR       plant .api-token.<pid> links for the next 2000
//!                                         pids plus api-token, all -> VICTIM; print 2000
//! devtool http-static ADDR PORT BODY      serve BODY (200, any path) until killed
//! devtool wer REF_FILE HYPOTHESIS         word error rate of HYPOTHESIS vs the file, 3 places
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match argv.as_slice() {
        ["free-port"] => free_port(),
        ["stale-uds", path] => stale_uds(path),
        ["plant-symlinks", victim, dir] => plant_symlinks(victim, dir),
        ["http-static", addr, port, body] => http_static(addr, port, body),
        ["wer", ref_file, hyp] => wer_cmd(ref_file, hyp),
        _ => Err(format!("usage: see {}", file!())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("devtool: {e}");
            ExitCode::from(2)
        }
    }
}

fn free_port() -> Result<(), String> {
    let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    println!("{port}");
    Ok(())
}

/// Bind then close: like a daemon that died without unlinking, the socket
/// inode stays on disk (Rust, unlike Python's `socket.close`, never unlinks).
fn stale_uds(path: &str) -> Result<(), String> {
    let l = UnixListener::bind(path).map_err(|e| format!("bind {path}: {e}"))?;
    drop(l);
    Ok(())
}

/// One process plants every link: a shell loop of `ln` would itself consume
/// the very pids it is trying to occupy.
fn plant_symlinks(victim: &str, dir: &str) -> Result<(), String> {
    const COUNT: u32 = 2000;
    let me = std::process::id();
    let dir = Path::new(dir);
    for pid in me + 1..=me + COUNT {
        symlink(victim, dir.join(format!(".api-token.{pid}"))).map_err(|e| e.to_string())?;
    }
    symlink(victim, dir.join("api-token")).map_err(|e| e.to_string())?;
    println!("{COUNT}");
    Ok(())
}

fn http_static(addr: &str, port: &str, body: &str) -> Result<(), String> {
    let l = TcpListener::bind(format!("{addr}:{port}")).map_err(|e| e.to_string())?;
    // The body is given with shell-friendly `\n` escapes.
    let body = body.replace("\\n", "\n");
    for conn in l.incoming().flatten() {
        let body = body.clone();
        std::thread::spawn(move || serve_one(conn, &body));
    }
    Ok(())
}

fn serve_one(mut conn: TcpStream, body: &str) {
    let Ok(read_half) = conn.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {}
        }
    }
    let reply = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = conn.write_all(reply.as_bytes());
    let _ = conn.flush();
}

/// Lowercase, keep `[a-z0-9' ]`, everything else becomes a separator.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Word-level Levenshtein distance over the reference length.
fn wer(reference: &str, hypothesis: &str) -> f64 {
    let (r, h) = (normalize(reference), normalize(hypothesis));
    let mut d: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut nd = vec![i + 1];
        for (j, hw) in h.iter().enumerate() {
            let sub = d[j] + usize::from(rw != hw);
            nd.push(sub.min(d[j + 1] + 1).min(nd[j] + 1));
        }
        d = nd;
    }
    d[h.len()] as f64 / r.len().max(1) as f64
}

fn wer_cmd(ref_file: &str, hyp: &str) -> Result<(), String> {
    let reference = std::fs::read_to_string(ref_file).map_err(|e| format!("{ref_file}: {e}"))?;
    println!("{:.3}", wer(&reference, hyp));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_scores_zero_regardless_of_case_and_punctuation() {
        assert_eq!(wer("Hello, World!", "hello world"), 0.0);
    }

    #[test]
    fn one_substitution_in_four_words_is_a_quarter() {
        assert_eq!(wer("a b c d", "a b x d"), 0.25);
    }

    #[test]
    fn insertion_and_deletion_count() {
        assert_eq!(wer("a b", "a b c"), 0.5);
        assert_eq!(wer("a b c d", "a b"), 0.5);
    }

    #[test]
    fn empty_reference_does_not_divide_by_zero() {
        assert_eq!(wer("", "x"), 1.0);
    }

    #[test]
    fn apostrophes_survive_normalisation() {
        assert_eq!(normalize("Don't STOP."), ["don't", "stop"]);
    }
}
