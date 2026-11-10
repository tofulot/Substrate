import { appendFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

// SemVer 2.0.0, including the restriction on leading zeroes in numeric prereleases.
const semver =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

export function releaseInfo({
  packageVersion,
  lockVersion,
  cargoVersion,
  tag = "",
}) {
  const match =
    typeof packageVersion === "string" && packageVersion.match(semver);
  if (!match || match[0] !== packageVersion)
    throw new Error("package.json must contain a valid semantic version");
  if (lockVersion !== packageVersion)
    throw new Error("package-lock.json version must match package.json");
  if (cargoVersion !== packageVersion)
    throw new Error("Cargo workspace version must match package.json");
  if (tag && tag !== `v${packageVersion}`)
    throw new Error(
      `Tag ${tag} must match v${packageVersion}; update the package and Cargo versions before tagging`,
    );
  return {
    version: packageVersion,
    tag: tag || `v${packageVersion}`,
    prerelease: String(Boolean(match[4])),
  };
}

export function cargoWorkspaceVersion(toml) {
  const section = toml.match(
    /^\[workspace\.package\]\s*\r?\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m,
  )?.[1];
  const version = section?.match(/^version\s*=\s*"([^"]+)"\s*(?:#.*)?$/m)?.[1];
  if (!version)
    throw new Error("Missing workspace.package.version in Cargo.toml");
  return version;
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    const root = new URL("../", import.meta.url);
    const pkg = JSON.parse(readFileSync(new URL("package.json", root), "utf8"));
    const lock = JSON.parse(
      readFileSync(new URL("package-lock.json", root), "utf8"),
    );
    const cargo = readFileSync(new URL("Cargo.toml", root), "utf8");
    if (lock.version !== lock.packages?.[""]?.version)
      throw new Error("package-lock.json root versions disagree");
    const info = releaseInfo({
      packageVersion: pkg.version,
      lockVersion: lock.version,
      cargoVersion: cargoWorkspaceVersion(cargo),
      tag: process.env.RELEASE_TAG || "",
    });
    if (process.env.GITHUB_OUTPUT)
      appendFileSync(
        process.env.GITHUB_OUTPUT,
        Object.entries(info)
          .map(([key, value]) => `${key}=${value}\n`)
          .join(""),
      );
    console.log(JSON.stringify(info));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
