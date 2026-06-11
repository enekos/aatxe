//! End-to-end CLI tests: compose `aatxe compare` and `aatxe report` against
//! fixture JSON. No network, no real runners — just exercise the binary.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn write_fixture(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn run_report(samples: &[f64], language: &str) -> String {
    let samples_json = serde_json::to_string(samples).unwrap();
    format!(
        r#"{{
            "schemaVersion": 2,
            "language": "{lang}",
            "service": "svc",
            "ref": "abcdef0123",
            "runner": "test",
            "startedAt": "2026-06-01T00:00:00Z",
            "finishedAt": "2026-06-01T00:00:01Z",
            "runs": [{{
                "name": "a",
                "file": "x.rs",
                "iterations": {n},
                "batchSize": 1,
                "elapsedNs": 0.0,
                "samples": {samples},
                "mean": 0.0, "median": 0.0, "trimmedMean": 0.0,
                "stddev": 0.0, "cv": 0.0, "mad": 0.0, "iqr": 0.0,
                "min": 0.0, "max": 0.0, "p50": 0.0, "p95": 0.0, "p99": 0.0
            }}]
        }}"#,
        lang = language,
        n = samples.len(),
        samples = samples_json,
    )
}

#[test]
fn compare_emits_json_and_markdown() {
    let tmp = TempDir::new().unwrap();
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = write_fixture(&tmp, "base.json", &run_report(&base_samples, "rust"));
    let head = write_fixture(&tmp, "head.json", &run_report(&head_samples, "rust"));
    let out_json = tmp.path().join("cmp.json");
    let out_md = tmp.path().join("cmp.md");

    let mut cmd = Command::cargo_bin("aatxe").unwrap();
    cmd.args([
        "compare",
        "--base",
        base.to_str().unwrap(),
        "--head",
        head.to_str().unwrap(),
        "--out",
        out_json.to_str().unwrap(),
        "--markdown",
        out_md.to_str().unwrap(),
        "--fail-on-regression",
    ]);
    let assert = cmd.assert();
    // 30% slower head ⇒ regression ⇒ exit 2.
    assert.code(2);

    let cmp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    assert_eq!(cmp["summary"]["regressions"], 1);
    let md = std::fs::read_to_string(&out_md).unwrap();
    assert!(md.starts_with("<!-- aatxe:report -->"));
    assert!(md.contains("Regression"));
}

#[test]
fn report_subcommand_renders_markdown_from_diff_json() {
    let tmp = TempDir::new().unwrap();
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = write_fixture(&tmp, "base.json", &run_report(&base_samples, "go"));
    let head = write_fixture(&tmp, "head.json", &run_report(&head_samples, "go"));
    let cmp_out = tmp.path().join("cmp.json");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            base.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--out",
            cmp_out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let md_out = tmp.path().join("report.md");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "report",
            "--diff",
            cmp_out.to_str().unwrap(),
            "--out",
            md_out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("wrote markdown"));
    let md = std::fs::read_to_string(&md_out).unwrap();
    assert!(md.contains("(Go)"));
}

#[test]
fn help_lists_all_subcommands() {
    Command::cargo_bin("aatxe")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("run"))
        .stdout(contains("compare"))
        .stdout(contains("report"))
        .stdout(contains("comment"))
        .stdout(contains("affected"))
        .stdout(contains("list"))
        .stdout(contains("baseline"));
}

#[test]
fn baseline_save_then_compare_against_local_gates_regression() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("baselines");
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.3).collect();
    let base = write_fixture(&tmp, "aatxe.json", &run_report(&base_samples, "rust"));
    let head = write_fixture(&tmp, "head.json", &run_report(&head_samples, "rust"));

    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "baseline",
            "save",
            "--report",
            base.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("saved baseline 'default'"));

    let out_json = tmp.path().join("cmp.json");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--against-local",
            "--baseline-dir",
            dir.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--out",
            out_json.to_str().unwrap(),
            "--fail-on-regression",
        ])
        .assert()
        .code(2)
        .stderr(contains("base = local baseline 'default'"));

    let cmp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    assert_eq!(cmp["summary"]["regressions"], 1);
}

#[test]
fn compare_against_local_without_saved_baseline_hints_at_save() {
    let tmp = TempDir::new().unwrap();
    let head_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head = write_fixture(&tmp, "head.json", &run_report(&head_samples, "rust"));
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--against-local",
            "--baseline-dir",
            tmp.path().to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(contains("aatxe baseline save"));
}

#[test]
fn compare_rejects_base_combined_with_against_local() {
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            "x.json",
            "--against-local",
            "--head",
            "y.json",
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

#[test]
fn compare_requires_base_or_against_local() {
    Command::cargo_bin("aatxe")
        .unwrap()
        .args(["compare", "--head", "y.json"])
        .assert()
        .failure()
        .stderr(contains("--base"));
}

#[test]
fn baseline_list_and_rm_round_trip() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("baselines");
    let samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let report = write_fixture(&tmp, "aatxe.json", &run_report(&samples, "go"));

    for name in ["default", "experiment"] {
        Command::cargo_bin("aatxe")
            .unwrap()
            .args([
                "baseline",
                "save",
                "--report",
                report.to_str().unwrap(),
                "--name",
                name,
                "--dir",
                dir.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    Command::cargo_bin("aatxe")
        .unwrap()
        .args(["baseline", "list", "--dir", dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("default"))
        .stdout(contains("experiment"));

    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "baseline",
            "rm",
            "--name",
            "experiment",
            "--dir",
            dir.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(!dir.join("experiment.json").exists());
    assert!(dir.join("default.json").exists());
}

#[test]
fn baseline_save_rejects_path_traversal_names() {
    let tmp = TempDir::new().unwrap();
    let samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let report = write_fixture(&tmp, "aatxe.json", &run_report(&samples, "ts"));
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "baseline",
            "save",
            "--report",
            report.to_str().unwrap(),
            "--name",
            "../escape",
            "--dir",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(contains("invalid baseline name"));
}

#[test]
fn compare_unchanged_with_fail_on_regression_exits_zero() {
    // Identical reports ⇒ no regression ⇒ exit 0 even when the gate is set.
    let tmp = TempDir::new().unwrap();
    let samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let base = write_fixture(&tmp, "a.json", &run_report(&samples, "rust"));
    let head = write_fixture(&tmp, "b.json", &run_report(&samples, "rust"));
    let out = tmp.path().join("cmp.json");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            base.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--fail-on-regression",
        ])
        .assert()
        .code(0);
    let cmp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(cmp["summary"]["regressions"], 0);
}

#[test]
fn compare_threshold_override_can_relax_gate() {
    // A 10% slowdown is a regression at the default 5% threshold but neutral
    // at a 15% threshold.
    let tmp = TempDir::new().unwrap();
    let base_samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let head_samples: Vec<f64> = base_samples.iter().map(|x| x * 1.10).collect();
    let base = write_fixture(&tmp, "a.json", &run_report(&base_samples, "rust"));
    let head = write_fixture(&tmp, "b.json", &run_report(&head_samples, "rust"));

    // Strict (default 5%) ⇒ exit 2.
    let strict_out = tmp.path().join("strict.json");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            base.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--out",
            strict_out.to_str().unwrap(),
            "--fail-on-regression",
        ])
        .assert()
        .code(2);

    // Loose 15% threshold ⇒ exit 0, verdict goes neutral.
    let loose_out = tmp.path().join("loose.json");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            base.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--threshold",
            "0.15",
            "--out",
            loose_out.to_str().unwrap(),
            "--fail-on-regression",
        ])
        .assert()
        .code(0);
    let cmp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&loose_out).unwrap()).unwrap();
    assert_eq!(cmp["summary"]["regressions"], 0);
}

#[test]
fn compare_invalid_json_errors_with_useful_message() {
    let tmp = TempDir::new().unwrap();
    let bad = write_fixture(&tmp, "bad.json", "{ not really json");
    let ok = write_fixture(
        &tmp,
        "ok.json",
        &run_report(&[1.0, 2.0, 3.0, 4.0, 5.0], "rust"),
    );
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            bad.to_str().unwrap(),
            "--head",
            ok.to_str().unwrap(),
            "--out",
            tmp.path().join("out.json").to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(contains("reading base report"));
}

#[test]
fn list_subcommand_finds_bench_files() {
    let tmp = TempDir::new().unwrap();
    let bench = tmp.path().join("a.bench.ts");
    std::fs::write(&bench, "import { bench } from '@aatxe/bench'\n").unwrap();
    // A regular .ts file that shouldn't match.
    std::fs::write(tmp.path().join("util.ts"), "export const X = 1\n").unwrap();
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "list",
            "--lang",
            "ts",
            "--cwd",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("a.bench.ts"))
        .stderr(contains("1 bench file(s)"));
}

#[test]
fn list_subcommand_excludes_node_modules() {
    let tmp = TempDir::new().unwrap();
    let nm = tmp.path().join("node_modules/pkg");
    std::fs::create_dir_all(&nm).unwrap();
    std::fs::write(nm.join("dep.bench.ts"), "").unwrap();
    std::fs::write(tmp.path().join("real.bench.ts"), "").unwrap();
    let assertion = Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "list",
            "--lang",
            "ts",
            "--cwd",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    assert!(stdout.contains("real.bench.ts"));
    assert!(
        !stdout.contains("dep.bench.ts"),
        "node_modules/dep.bench.ts must not be listed; got: {stdout}"
    );
}

#[test]
fn report_subcommand_errors_on_missing_input() {
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "report",
            "--diff",
            tmp.path().join("nonexistent.json").to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(contains("reading"));
}

#[test]
fn compare_writes_markdown_with_sticky_marker() {
    let tmp = TempDir::new().unwrap();
    let samples: Vec<f64> = (100..160).map(|x| x as f64).collect();
    let base = write_fixture(&tmp, "a.json", &run_report(&samples, "rust"));
    let head = write_fixture(&tmp, "b.json", &run_report(&samples, "rust"));
    let md_out = tmp.path().join("body.md");
    Command::cargo_bin("aatxe")
        .unwrap()
        .args([
            "compare",
            "--base",
            base.to_str().unwrap(),
            "--head",
            head.to_str().unwrap(),
            "--out",
            tmp.path().join("cmp.json").to_str().unwrap(),
            "--markdown",
            md_out.to_str().unwrap(),
        ])
        .assert()
        .success();
    let md = std::fs::read_to_string(&md_out).unwrap();
    assert!(md.starts_with("<!-- aatxe:report -->"));
}
