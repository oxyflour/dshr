// 把本机 DSH 桌面版内置的 Node 运行时复制到 <repo>/runtime/node（gitignored）。
//
// 为什么不从网络下载：PLAN §5.5 要求"捆绑固定版本、离线可运行、与 sidecar lockfile
// 同批升级"。本机 DSH 安装里的 node 就是用户机器上真正运行的那份（0.2.2-rc.1 自带
// Node 24.21.0），复制它能保证版本一致，且企业内网/离线环境不需要 npm registry。
//
// 用法：
//   node tools/vendor-node.mjs                 # 自动探测并复制
//   node tools/vendor-node.mjs --check         # 只检查 runtime/node 是否存在
//   node tools/vendor-node.mjs --force         # 已存在也覆盖（升级 runtime 时）
//   node tools/vendor-node.mjs --source <path> # 指定 node.exe 或 DSH resources 目录
//   node tools/vendor-node.mjs --dest <dir>    # 指定目标（默认 runtime/node）
//
// 探测顺序：
//   1. --source / DSHR_NODE_SOURCE / DSHR_NODE
//   2. DSHR_ASAR 同级 resources/runtime/primary-runtime/dependencies/node/bin/node.exe
//   3. %LOCALAPPDATA%\Programs\DeepSeek Harness\resources\...
//   4. %ProgramFiles%\DeepSeek Harness\resources\...
//   5. PATH 上的 node（最后兜底）
//
// 产物（不提交）：
//   runtime/node/node.exe    Node 可执行文件
//   runtime/node/RUNTIME.json  版本/来源/平台，供安装确认与排障

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.dirname(scriptDir);
const exeName = process.platform === "win32" ? "node.exe" : "node";

function parseArgs(argv) {
  const options = { force: false, check: false, source: undefined, dest: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--force") options.force = true;
    else if (arg === "--check") options.check = true;
    else if (arg === "--source") options.source = argv[++index];
    else if (arg.startsWith("--source=")) options.source = arg.slice("--source=".length);
    else if (arg === "--dest") options.dest = argv[++index];
    else if (arg.startsWith("--dest=")) options.dest = arg.slice("--dest=".length);
    else if (arg === "--help" || arg === "-h") {
      console.log("usage: node tools/vendor-node.mjs [--check] [--force] [--source <path>] [--dest <dir>]");
      process.exit(0);
    } else {
      console.error(`[vendor-node] 未知参数: ${arg}`);
      process.exit(2);
    }
  }
  return options;
}

/** DSH 安装的 resources 目录候选。 */
function resourcesRoots() {
  const roots = [];
  if (process.env.DSHR_ASAR) roots.push(path.dirname(process.env.DSHR_ASAR));
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) {
    roots.push(path.join(localAppData, "Programs", "DeepSeek Harness", "resources"));
    roots.push(path.join(localAppData, "DeepSeek Harness", "resources"));
  }
  const programFiles = process.env.ProgramFiles || process.env.ProgramW6432;
  if (programFiles) {
    roots.push(path.join(programFiles, "DeepSeek Harness", "resources"));
  }
  return roots;
}

/** 在一个目录里按已知布局找 node 可执行文件。 */
function probeDirectory(directory) {
  const candidates = [
    path.join(directory, exeName),
    path.join(directory, "bin", exeName),
    path.join(directory, "node", exeName),
    path.join(directory, "node", "bin", exeName),
    path.join(directory, "dependencies", "node", "bin", exeName),
    path.join(directory, "runtime", "node", exeName),
    path.join(directory, "runtime", "primary-runtime", "dependencies", "node", "bin", exeName),
    path.join(directory, "primary-runtime", "dependencies", "node", "bin", exeName),
  ];
  for (const candidate of candidates) {
    try {
      if (fs.statSync(candidate).isFile()) return candidate;
    } catch {
      // 继续找下一个布局。
    }
  }
  return null;
}

/** 把候选路径解析为真实的 node 可执行文件（候选可以是文件或目录）。 */
function resolveCandidate(candidate) {
  if (!candidate) return null;
  try {
    const stat = fs.statSync(candidate);
    if (stat.isFile()) return candidate;
    if (stat.isDirectory()) return probeDirectory(candidate);
  } catch {
    // 不存在：继续尝试下一个候选。
  }
  return null;
}

/** 收集所有候选 node 路径（去重，保持优先级）。 */
function nodeCandidates(options) {
  const explicit = options.source || process.env.DSHR_NODE_SOURCE || process.env.DSHR_NODE;
  const candidates = [];
  if (explicit) candidates.push(explicit);

  for (const resources of resourcesRoots()) {
    candidates.push(
      path.join(resources, "runtime", "primary-runtime", "dependencies", "node", "bin", exeName),
      path.join(resources, "runtime", "primary-runtime", "dependencies", "node", exeName),
      path.join(resources, "runtime", "bin", exeName),
    );
  }
  if (process.env.DSHR_DSH_SOURCE) {
    candidates.push(process.env.DSHR_DSH_SOURCE);
  }
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    if (directory) candidates.push(path.join(directory, exeName));
  }
  return [...new Set(candidates)];
}

function findNode(options) {
  const searched = [];
  for (const candidate of nodeCandidates(options)) {
    searched.push(candidate);
    const resolved = resolveCandidate(candidate);
    if (resolved) return { program: resolved, searched };
  }
  return { program: null, searched };
}

function readVersion(program) {
  try {
    return execFileSync(program, ["--version"], {
      encoding: "utf8",
      timeout: 15_000,
      windowsHide: true,
    }).trim();
  } catch (error) {
    return `<读取失败: ${error.message}>`;
  }
}

function readRuntimeManifest(destDir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(destDir, "RUNTIME.json"), "utf8"));
  } catch {
    return null;
  }
}

function writeRuntimeManifest(destDir, source, version) {
  const manifest = {
    schemaVersion: 1,
    node: version,
    source,
    platform: process.platform,
    arch: process.arch,
    vendoredAt: new Date().toISOString(),
  };
  fs.writeFileSync(path.join(destDir, "RUNTIME.json"), JSON.stringify(manifest, null, 2) + "\n");
  return manifest;
}

function printCheck(destDir) {
  const program = path.join(destDir, exeName);
  if (!fs.existsSync(program)) {
    console.log(`[vendor-node] 未找到 ${program}`);
    console.log("[vendor-node] 运行 `node tools/vendor-node.mjs` 生成（需要本机安装 DSH 桌面版）");
    return 1;
  }
  const manifest = readRuntimeManifest(destDir);
  if (manifest) {
    console.log(`[vendor-node] ${program}`);
    console.log(`[vendor-node] node=${manifest.node} source=${manifest.source} vendoredAt=${manifest.vendoredAt}`);
  } else {
    console.log(`[vendor-node] ${program}（RUNTIME.json 缺失，来源不可审计）`);
  }
  return 0;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const destDir = path.resolve(options.dest || path.join(repoRoot, "runtime", "node"));

  if (options.check) {
    process.exit(printCheck(destDir));
  }

  const destination = path.join(destDir, exeName);
  if (fs.existsSync(destination) && !options.force) {
    const manifest = readRuntimeManifest(destDir);
    console.log(`[vendor-node] 已存在：${destination}`);
    if (manifest) console.log(`[vendor-node] node=${manifest.node}（--force 可覆盖）`);
    console.log("[vendor-node] 提示：升级 Node 时先删除 runtime/node 再重新 vendor");
    return;
  }

  const { program, searched } = findNode(options);
  if (!program) {
    console.error("[vendor-node] 没有找到可用的 Node 运行时。已尝试：");
    for (const candidate of searched) console.error(`  - ${candidate}`);
    console.error("[vendor-node] 解决方式：安装 DeepSeek Harness 桌面版，或 `--source <node.exe>` 指定路径。");
    process.exit(1);
  }

  const version = readVersion(program);
  fs.mkdirSync(destDir, { recursive: true });
  fs.copyFileSync(program, destination);
  const manifest = writeRuntimeManifest(destDir, program, version);
  const size = fs.statSync(destination).size;
  console.log(`[vendor-node] ${program} -> ${destination}`);
  console.log(`[vendor-node] node=${manifest.node} platform=${manifest.platform} arch=${manifest.arch} size=${(size / 1024 / 1024).toFixed(1)} MiB`);
  console.log("[vendor-node] 解析顺序：DSHR_NODE → runtime/node → PATH（见 docs/m2-runtime.md）");
}

main();
