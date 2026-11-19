import { isBuiltin } from "node:module";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = fileURLToPath(new URL("../", import.meta.url));
const electronDir = resolve(root, "electron");
const configPath = resolve(root, "tsconfig.json");
const config = ts.readConfigFile(configPath, ts.sys.readFile);
if (config.error) {
  throw new Error(
    ts.flattenDiagnosticMessageText(config.error.messageText, "\n"),
  );
}
const { options } = ts.parseJsonConfigFileContent(config.config, ts.sys, root);
const resolutionCache = ts.createModuleResolutionCache(
  root,
  ts.sys.useCaseSensitiveFileNames
    ? (path) => path
    : (path) => path.toLowerCase(),
  options,
);
const files = ts.sys.readDirectory(
  root,
  [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"],
  undefined,
  ["src/**/*", "shared/**/*"],
);

function isInElectron(path) {
  const fromElectron = relative(electronDir, path);
  return (
    fromElectron !== ".." &&
    !fromElectron.startsWith(`..${sep}`) &&
    !isAbsolute(fromElectron)
  );
}

function crossesBoundary(specifier, file) {
  const name = specifier.split("?", 1)[0];
  if (
    name === "electron" ||
    name.startsWith("electron/") ||
    name.startsWith("node:") ||
    isBuiltin(name)
  ) {
    return true;
  }

  // Vite supports root-relative paths and resource queries such as ?raw.
  if (name.startsWith("/") && isInElectron(resolve(root, `.${name}`)))
    return true;
  if (/^\.\.?\//.test(name) && isInElectron(resolve(dirname(file), name)))
    return true;
  const { resolvedModule } = ts.resolveModuleName(
    name,
    file,
    options,
    ts.sys,
    resolutionCache,
  );
  return (
    resolvedModule !== undefined &&
    isInElectron(resolvedModule.resolvedFileName)
  );
}

for (const file of files.sort()) {
  const source = ts.sys.readFile(file);
  if (source === undefined) throw new Error(`Could not read ${file}`);
  for (const { fileName, pos } of ts.preProcessFile(source, true, true)
    .importedFiles) {
    if (!crossesBoundary(fileName, file)) continue;
    const line = source.slice(0, pos).split(/\r\n|\n|\r/).length;
    const location = `${relative(root, file)}:${line}`;
    console.error(
      `${location}: ${JSON.stringify(fileName)} crosses the Electron import boundary. Keep native code in electron/ and access it through the typed window.substrate bridge.`,
    );
    process.exitCode = 1;
  }
}
