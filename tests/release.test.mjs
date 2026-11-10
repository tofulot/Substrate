import { test } from "node:test";
import assert from "node:assert/strict";
import {
  releaseInfo,
  cargoWorkspaceVersion,
} from "../scripts/release-info.mjs";

const versions = (version, tag = `v${version}`) => ({
  packageVersion: version,
  lockVersion: version,
  cargoVersion: version,
  tag,
});

test("stable version tags produce normal releases", () => {
  assert.deepEqual(releaseInfo(versions("1.2.3")), {
    version: "1.2.3",
    tag: "v1.2.3",
    prerelease: "false",
  });
});
test("prerelease tags retain the full version and prerelease flag", () => {
  assert.equal(releaseInfo(versions("0.2.0-rc.1")).prerelease, "true");
  assert.equal(
    releaseInfo(versions("1.2.3-alpha.2+build.42")).prerelease,
    "true",
  );
});
test("build metadata containing a hyphen does not make a stable version a prerelease", () => {
  assert.equal(releaseInfo(versions("1.2.3+build-42")).prerelease, "false");
});
test("manual build uses the checked-out package version", () => {
  assert.equal(releaseInfo(versions("0.1.0", "")).tag, "v0.1.0");
});
test("tag, npm lock, and Cargo version mismatches are rejected", () => {
  assert.throws(() => releaseInfo(versions("0.1.0", "v0.2.0")), /Tag/);
  assert.throws(
    () => releaseInfo({ ...versions("0.1.0"), lockVersion: "0.2.0" }),
    /lock/,
  );
  assert.throws(
    () => releaseInfo({ ...versions("0.1.0"), cargoVersion: "0.2.0" }),
    /Cargo/,
  );
});
test("invalid versions cannot enter filenames, shell commands, or Actions outputs", () => {
  for (const version of [
    "01.2.3",
    "1.2.3\n",
    "1.2.3\r\n",
    "1.2",
    "1.2.3-01",
    "1.2.3-rc.01",
    "1.2.3-",
    "v1.2.3",
    "1.2.3\nwrite=true",
    "1.2.3;echo unsafe",
    "../1.2.3",
  ]) {
    assert.throws(
      () => releaseInfo(versions(version)),
      /semantic version/,
      version,
    );
  }
});
test("only the workspace package version is read from Cargo", () => {
  const source =
    '[workspace]\nresolver = "2"\n[workspace.package]\nversion = "1.2.3" # release\nedition = "2024"\n[workspace.dependencies]\nother = { version = "9" }\n';
  assert.equal(cargoWorkspaceVersion(source), "1.2.3");
  assert.equal(
    cargoWorkspaceVersion('[workspace.package]\nversion = "1.2.3"'),
    "1.2.3",
  );
  assert.throws(
    () => cargoWorkspaceVersion('[package]\nversion = "1.2.3"'),
    /Missing/,
  );
});
