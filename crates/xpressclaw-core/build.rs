use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=XPRESSCLAW_RUNNER_TAG");

    let tag = env::var("XPRESSCLAW_RUNNER_TAG").unwrap_or_else(|_| "latest".to_string());
    assert!(
        !tag.is_empty()
            && tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "XPRESSCLAW_RUNNER_TAG must be a non-empty container tag"
    );
    println!("cargo:rustc-env=XPRESSCLAW_RUNNER_TAG={tag}");
}
