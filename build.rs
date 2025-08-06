// Copyright (C) 2023  Host Mobility AB

// This file is part of HOST Insight Client

// HOST Insight Client is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// HOST Insight Client is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program; if not, write to the Free Software Foundation,
// Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA

use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Include the output of git describe --tags --dirty
    let git_describe_output = Command::new("git")
        .args(["describe", "--tags", "--dirty"])
        .output()?
        .stdout;

    let git_version = String::from_utf8(git_describe_output)?.trim().to_string();

    println!("cargo:rustc-env=GIT_VERSION={}", git_version);
    println!("cargo:rustc-env=BIN_DIR=/opt/host-insight-client");
    println!("cargo:rustc-env=CONF_DIR=/etc/opt/host-insight-client");

    // Build proto
    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");

    tonic_build::configure().compile_protos_with_config(
        config,
        &[
            "proto/host_insight.proto",
            "proto/host_insight_controller.proto",
            "proto/host_insight_enums.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
