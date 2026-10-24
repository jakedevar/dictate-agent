use std::io::Write;

fn main() {
    // Accept=yes + StandardInput=socket gives this connection stdin/stdout.
    // Emit immediately: the optional {"op":"status"} line is not required.
    let status = dictate_gpu_gate::status::observe(&Default::default());
    let _ = writeln!(std::io::stdout().lock(), "{status}");
}
