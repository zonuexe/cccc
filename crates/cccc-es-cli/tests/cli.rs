use assert_cmd::Command;

#[test]
fn version_reports_binary_name() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::starts_with("cccc-es "));
}

#[test]
fn nonexistent_path_is_an_error() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .arg("/no/such/path-xyz")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn existing_dir_with_no_matches_is_ok() {
    // A real directory containing nothing analyzable is an empty, successful run
    // — distinct from a path that doesn't exist.
    let dir = std::env::temp_dir().join("cccc_es_empty_match_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("readme.md"), "# not analyzed").unwrap();
    Command::cargo_bin("cccc-es")
        .unwrap()
        .arg(&dir)
        .assert()
        .success();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outputs_json_by_default() {
    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .arg("tests/fixtures/sample.ts")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let file = &v["files"][0];
    assert_eq!(file["path"], "tests/fixtures/sample.ts");
    let funcs = file["functions"].as_array().unwrap();
    let sop = funcs.iter().find(|f| f["name"] == "sumOfPrimes").unwrap();
    assert_eq!(sop["cognitive"], 7);
    assert_eq!(sop["cyclomatic"], 4);
}

#[test]
fn jobs_option_produces_same_output() {
    // The result must be independent of the worker count.
    let single = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--jobs", "1", "tests/fixtures/sample.ts"])
        .assert()
        .success();
    let many = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["-j", "4", "tests/fixtures/sample.ts"])
        .assert()
        .success();
    assert_eq!(
        single.get_output().stdout,
        many.get_output().stdout,
        "output must not depend on --jobs"
    );
}

#[test]
fn jobs_zero_is_rejected() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--jobs", "0", "tests/fixtures/sample.ts"])
        .assert()
        .failure();
}

#[test]
fn includes_project_summary() {
    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .arg("tests/fixtures/sample.ts")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let s = &v["summary"];
    assert_eq!(s["file_count"], 1);
    // sample.ts has sumOfPrimes, getWords, classify.
    assert_eq!(s["function_count"], 3);
    assert_eq!(s["cognitive"]["sum"], 10);
    assert_eq!(s["cognitive"]["max"], 7);
    assert!(s["cognitive"]["median"].is_number());
    assert!(s["cyclomatic"]["p95"].is_number());
}

#[test]
fn top_cognitive_returns_flat_ranking() {
    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--top-cognitive", "2", "tests/fixtures/sample.ts"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(v.get("files").is_none(), "top mode must not emit files");
    assert_eq!(v["metric"], "cognitive");
    let top = v["top"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    // sample.ts: sumOfPrimes(7) > classify(2) > getWords(1).
    assert_eq!(top[0]["name"], "sumOfPrimes");
    assert_eq!(top[0]["cognitive"], 7);
    assert_eq!(top[0]["path"], "tests/fixtures/sample.ts");
    assert_eq!(top[1]["name"], "classify");
    // summary still reflects the full population (3 functions).
    assert_eq!(v["summary"]["function_count"], 3);
}

#[test]
fn top_cyclomatic_orders_by_cyclomatic() {
    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--top-cyclomatic", "1", "tests/fixtures/sample.ts"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(v["metric"], "cyclomatic");
    // sumOfPrimes has the highest cyclomatic (4).
    assert_eq!(v["top"][0]["name"], "sumOfPrimes");
    assert_eq!(v["top"][0]["cyclomatic"], 4);
}

#[test]
fn top_flags_are_mutually_exclusive() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args([
            "--top-cognitive",
            "1",
            "--top-cyclomatic",
            "1",
            "tests/fixtures/sample.ts",
        ])
        .assert()
        .failure();
}

#[test]
fn exclude_glob_drops_matching_files() {
    // Two analyzable files in a temp dir; excluding `*.test.ts` by file name
    // must leave only the non-test one.
    let dir = std::env::temp_dir().join("cccc_es_exclude_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/app.ts"), "function a() { return 1; }").unwrap();
    std::fs::write(dir.join("src/app.test.ts"), "function b() { return 2; }").unwrap();

    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--exclude", "*.test.ts"])
        .arg(&dir)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "the *.test.ts file must be excluded");
    assert!(
        files[0]["path"].as_str().unwrap().ends_with("app.ts"),
        "only app.ts should remain"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exclude_glob_prunes_a_directory_subtree() {
    let dir = std::env::temp_dir().join("cccc_es_exclude_dir_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dist/nested")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/app.ts"), "function a() { return 1; }").unwrap();
    std::fs::write(dir.join("dist/bundle.ts"), "function b() { return 2; }").unwrap();
    std::fs::write(dir.join("dist/nested/x.ts"), "function c() { return 3; }").unwrap();

    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--exclude", "dist/**"])
        .arg(&dir)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "everything under dist/ must be excluded");
    assert!(files[0]["path"].as_str().unwrap().ends_with("app.ts"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exclude_can_be_repeated() {
    let dir = std::env::temp_dir().join("cccc_es_exclude_multi_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.ts"), "function a() { return 1; }").unwrap();
    std::fs::write(dir.join("a.test.ts"), "function b() { return 2; }").unwrap();
    std::fs::write(dir.join("a.spec.ts"), "function c() { return 3; }").unwrap();

    let out = Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--exclude", "*.test.ts", "--exclude", "*.spec.ts"])
        .arg(&dir)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(v["files"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exclude_applies_to_explicit_file_argument() {
    // Even a file named explicitly on the command line is dropped if it matches.
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--exclude", "*.ts", "tests/fixtures/sample.ts"])
        .assert()
        .success()
        .stderr(predicates::str::contains("no matching files"));
}

#[test]
fn invalid_exclude_pattern_is_an_error() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--exclude", "a[b", "tests/fixtures/sample.ts"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn max_cognitive_threshold_fails() {
    // sumOfPrimes has cognitive 7, so a max of 5 must fail (exit 1).
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--max-cognitive", "5", "tests/fixtures/sample.ts"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn max_cognitive_threshold_passes_when_under() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--max-cognitive", "100", "tests/fixtures/sample.ts"])
        .assert()
        .success();
}

#[test]
fn table_output_renders() {
    Command::cargo_bin("cccc-es")
        .unwrap()
        .args(["--table", "tests/fixtures/sample.ts"])
        .assert()
        .success()
        .stdout(predicates::str::contains("sumOfPrimes"))
        .stdout(predicates::str::contains("file total"))
        .stdout(predicates::str::contains("summary"));
}
