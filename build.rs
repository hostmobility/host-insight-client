// Copyright (C) 2023  Host Mobility AB

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program; if not, write to the Free Software Foundation,
// Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA

use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Include the output of git describe --tags --long --dirty
    let git_describe_output = Command::new("git")
        .args(["describe", "--tags", "--long", "--dirty"])
        .output()
        .unwrap();
    let git_version = String::from_utf8(git_describe_output.stdout).unwrap();
    println!("cargo:rustc-env=GIT_VERSION={}", git_version);
    // Build proto
    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");
    tonic_build::configure().compile_with_config(
        config,
        &[
            "proto/ada.proto",
            "proto/ada_controller.proto",
            "proto/ada_enums.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
