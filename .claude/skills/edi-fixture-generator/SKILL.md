---
name: edi-fixture-generator
description: Generate EDI test fixture Excel files (.xlsx) for ClinMetric testing. Use when creating test data for the Excel parser, adding new test patients, or generating fixtures with specific patterns (improving, stable, variable, worsening). Triggers: "create test fixture", "generate test patient", "add test data", "make fixture file", "test xlsx".
---

# EDI Fixture Generator

Generate properly formatted Excel test fixtures matching the Microsoft Forms EDI export structure for ClinMetric testing.

## Quick Start

Generate a fixture using the script:

```bash
python3 .claude/skills/edi-fixture-generator/scripts/generate_fixture.py \
    <first_name> <last_name> <sessions> [options]
```

**Options:**
- `--report-type`: `both` | `caregiver` | `self-report` (default: both)
- `--pattern`: `improving` | `stable` | `variable` | `worsening` (default: improving)
- `--output`: Output directory (default: tests/fixtures)
- `--seed`: Random seed for reproducibility

## Examples

```bash
# 50-session patient with both report types, improving pattern
python3 .claude/skills/edi-fixture-generator/scripts/generate_fixture.py \
    Hannah Barbara 50 --seed 42

# Caregiver-only stable pattern
python3 .claude/skills/edi-fixture-generator/scripts/generate_fixture.py \
    Marcus Chen 6 --report-type caregiver --pattern stable

# Self-report only with variable scores
python3 .claude/skills/edi-fixture-generator/scripts/generate_fixture.py \
    Emma Rodriguez 12 --report-type self-report --pattern variable
```

## Adding Rust Tests

After generating a fixture, add a test to `src-tauri/src/excel_parser.rs`:

```rust
#[test]
fn test_fixture_<name>() {
    let path = fixture_path("TF_<First>_<Last>.xlsx");
    if !std::path::Path::new(&path).exists() {
        println!("Skipping test: fixture not found at {}", path);
        return;
    }

    let patient = parse_excel_file(&path).expect("Should parse TF_<First>_<Last>.xlsx");

    assert_eq!(patient.first_name, "<First>");
    assert_eq!(patient.last_name, "<Last>");
    assert_eq!(patient.sessions.len(), <N>, "Expected <N> sessions");

    // Verify report types based on --report-type used
    assert!(patient.has_caregiver_report, "Should have caregiver report");
    assert!(patient.has_self_report, "Should have self-report");
}
```

## Pattern Reference

| Pattern | Start Scores | End Scores | Use Case |
|---------|-------------|------------|----------|
| `improving` | High (3-4) | Low (0-1) | Treatment success |
| `worsening` | Low (0-1) | High (3-4) | Deterioration |
| `stable` | Medium (2) | Medium (2) | No change baseline |
| `variable` | Oscillating | Oscillating | Inconsistent responder |

## File Structure

Generated files follow the Microsoft Forms export format (Example_Data.xlsx):
- Columns A-I: Metadata (ID, dates, names, initials, reporter type)
- Columns J-V: 13 caregiver EDI items (indices 9-21)
- Columns W-AI: 13 self-report EDI items (indices 22-34)
- Naming: `TF_<First>_<Last>.xlsx`
- Location: `tests/fixtures/`
