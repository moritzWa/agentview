use std::fs;
use std::path::Path;

use agentview::domain::Provider;
use serde_json::Value;

fn public_harness_names() -> Vec<String> {
    let catalog_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/harnesses.json");
    let catalog: Value = serde_json::from_str(
        &fs::read_to_string(catalog_path).expect("read public harness catalog"),
    )
    .expect("parse public harness catalog");

    catalog["codingHarnesses"]
        .as_array()
        .expect("codingHarnesses array")
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("coding harness name")
                .to_owned()
        })
        .collect()
}

fn read_readme() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    fs::read_to_string(path)
        .expect("read README")
        .replace("\r\n", "\n")
}

fn disclosure_containing<'a>(readme: &'a str, marker: &str) -> &'a str {
    let marker_offset = readme.find(marker).expect("disclosure marker");
    let details_start = readme[..marker_offset]
        .rfind("<details>")
        .expect("opening disclosure");
    let details_end = readme[marker_offset..]
        .find("</details>")
        .map(|offset| marker_offset + offset)
        .expect("closed disclosure");
    &readme[details_start..details_end]
}

fn feature_matrix(readme: &str) -> &str {
    let start = readme
        .find("| Harness | Launch | Model / shell picker |")
        .expect("README feature matrix");
    let end = readme[start..]
        .find("\n\n")
        .map(|offset| start + offset)
        .expect("end of README feature matrix");
    &readme[start..end]
}

#[test]
fn readme_status_badges_match_release_metadata() {
    let readme = read_readme();
    let release_badge = format!(
        "https://img.shields.io/badge/release-v{}-506CF7.svg",
        env!("CARGO_PKG_VERSION")
    );

    assert!(
        readme.contains("https://img.shields.io/badge/tests-verified-506CF7.svg"),
        "README test badge must report the documented manual verification gate"
    );
    assert!(
        readme.contains(&release_badge),
        "README release badge must match the crate version"
    );
    assert!(
        !readme.contains("actions/workflows/ci.yml/badge.svg"),
        "README must not present hosted-runner availability as the test result"
    );
    assert!(
        !readme.contains("img.shields.io/github/v/release"),
        "README intentionally uses its version-matched static release badge"
    );
}

#[test]
fn readme_keeps_the_feature_matrix_openly_visible() {
    let readme = read_readme();
    let heading = readme
        .find("### Compare feature support by harness")
        .expect("visible feature comparison heading");
    assert_eq!(
        readme[..heading].matches("<details>").count(),
        readme[..heading].matches("</details>").count(),
        "feature comparison must not be hidden in a disclosure"
    );
    let matrix = feature_matrix(&readme);

    for harness in public_harness_names()
        .into_iter()
        .chain(["Terminal".to_owned()])
    {
        assert!(
            matrix.contains(&format!("| {harness} |")),
            "feature matrix is missing {harness}"
        );
    }
}

#[test]
fn readme_collapses_the_windows_install_command() {
    let readme = read_readme();
    let details = disclosure_containing(&readme, "Windows PowerShell");

    assert!(details.contains(
        "irm https://raw.githubusercontent.com/moritzWa/agentview/main/install.ps1 | iex"
    ));
    assert!(!readme.contains("## Quick start"));
}

#[test]
fn readme_advertises_only_the_current_short_command() {
    let readme = read_readme();
    assert!(readme.contains("shorter `av` command"));
}

#[test]
fn product_readme_and_catalog_share_one_exact_harness_inventory() {
    let catalog_names = public_harness_names();
    let product_names = Provider::CODING_HARNESSES
        .iter()
        .map(|provider| provider.public_name().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        catalog_names, product_names,
        "the harness catalog must match the product picker exactly"
    );
    assert_eq!(catalog_names.len(), Provider::CODING_HARNESS_COUNT);

    let readme = read_readme();
    assert!(readme.contains(&format!(
        "brings {} local coding harnesses plus Terminal",
        Provider::CODING_HARNESS_COUNT
    )));
    assert!(
        readme.contains(&format!(
            "Works with {} coding\nharnesses",
            Provider::CODING_HARNESS_COUNT
        )),
        "README tagline must state the exact coding-harness count"
    );

    let matrix_names = feature_matrix(&readme)
        .lines()
        .skip(2)
        .map(|row| row.split('|').nth(1).expect("README harness cell").trim())
        .collect::<Vec<_>>();
    let expected_names = catalog_names
        .iter()
        .map(String::as_str)
        .chain(["Terminal"])
        .collect::<Vec<_>>();
    assert_eq!(
        matrix_names, expected_names,
        "README feature rows must match the product picker exactly, followed by Terminal"
    );
}
