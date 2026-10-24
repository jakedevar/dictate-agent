use std::io::Write;

fn main() {
    let outcome = if std::env::args().skip(1).collect::<Vec<_>>() == ["acquire"] {
        match dictate_gpu_gate::gate::GateConfig::from_env() {
            Ok(config) => dictate_gpu_gate::gate::acquire(&config),
            Err(_) => "gate_config_error warning=fail_open",
        }
    } else {
        "gate_usage_error warning=fail_open expected=acquire"
    };
    // Do not panic on a closed journal pipe: every normal path exits zero.
    let _ = writeln!(std::io::stderr().lock(), "dictate-gpu-gate {outcome}");
}
