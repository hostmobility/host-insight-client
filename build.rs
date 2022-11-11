use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Include the output of git describe --tags --long --dirty
    let git_describe_output = Command::new("git")
        .args(&["describe", "--tags", "--long", "--dirty"])
        .output()
        .unwrap();
    let git_version = String::from_utf8(git_describe_output.stdout).unwrap();
    println!("cargo:rustc-env=GIT_VERSION={}", git_version);
    // Build proto
    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");
    tonic_build::configure().compile_with_config(config, &["proto/elevator.proto"], &["proto"])?;
    Ok(())
}
