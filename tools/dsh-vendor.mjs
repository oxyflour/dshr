// 从本机安装的 DeepSeek Harness app.asar 中抽取官方插件所需的 node_modules 闭包。
//
// 为什么不用 npm：官方包的版本是 0.2.2-rc.1（随桌面版发布），npm 上的版本与它不一定对齐；
// 直接抽本机安装的副本，保证「装载的是用户机器上真正运行的那份插件」。
//
// 用法：node tools/dsh-vendor.mjs [目标目录] [包名...]
// 默认目标目录：sidecar/dsh-official/node_modules
// 默认根包：workspace-dependencies + 只读 fs 工具
//
// 产物是 gitignored 的（node_modules/），需要本机安装 DSH 才能生成。

import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { lookup, openAsar, readFile, walk } from "./lib/asar.mjs";

const ASAR =
  process.env.DSHR_ASAR ||
  path.join(os.homedir(), "AppData/Local/Programs/DeepSeek Harness/resources/app.asar");

const [destArg, ...rootArgs] = process.argv.slice(2);
const dest = path.resolve(destArg || "sidecar/dsh-official/node_modules");
const roots = rootArgs.length
  ? rootArgs
  : [
      "@deepseek-ai/dsh-tool-workspace-dependencies",
      "@deepseek-ai/dsh-tool-fs",
    ];

const { fd, header, base } = openAsar(ASAR);
const unpackedRoot = path.join(path.dirname(ASAR), "app.asar.unpacked");
try {
  const seen = new Set();
  const missing = new Set();
  const queue = [...roots];
  let extracted = 0;

  while (queue.length) {
    const name = queue.shift();
    if (seen.has(name)) continue;
    seen.add(name);

    // 官方包在 asar 的 dsh/node_modules 下；少数前置包在根 node_modules。
    let packagePath = `dsh/node_modules/${name}`;
    let node = lookup(header, packagePath);
    if (!node) {
      packagePath = `node_modules/${name}`;
      node = lookup(header, packagePath);
    }
    if (!node) {
      missing.add(name);
      continue;
    }

    const files = [];
    walk(node, packagePath, files);
    const manifestPath = `${packagePath}/package.json`;
    const pkgNode = lookup(header, manifestPath);
    if (!pkgNode) {
      missing.add(name);
      continue;
    }
    const manifest = pkgNode.unpacked
      ? fs.readFileSync(path.join(unpackedRoot, manifestPath))
      : readFile(fd, base, pkgNode);
    const pkg = JSON.parse(manifest.toString("utf8"));
    const deps = {
      ...(pkg.dependencies || {}),
      ...(pkg.peerDependencies || {}),
      ...(pkg.optionalDependencies || {}),
    };
    for (const dep of Object.keys(deps)) queue.push(dep);

    for (const file of files) {
      // Some dependencies import runtime modules from src/ (for example restructure).
      if (file.endsWith(".map")) continue;
      const entry = lookup(header, file);
      // asar 里的符号链接没有 size/offset，跳过（运行时不需要 .bin 链接）。
      if (!entry || entry.link !== undefined || entry.size === undefined) continue;
      const outPath = path.join(dest, name, file.slice(packagePath.length + 1));
      fs.mkdirSync(path.dirname(outPath), { recursive: true });
      if (entry.unpacked) {
        // 原生模块不存 asar 内部，真实文件在 app.asar.unpacked 下。
        const unpacked = path.join(unpackedRoot, file);
        if (fs.existsSync(unpacked)) {
          fs.copyFileSync(unpacked, outPath);
          extracted += 1;
        }
        continue;
      }
      fs.writeFileSync(outPath, readFile(fd, base, entry));
      extracted += 1;
    }
  }

  console.log(`asar: ${ASAR}`);
  console.log(`dest: ${dest}`);
  console.log(`packages: ${seen.size - missing.size}, files: ${extracted}`);
  if (missing.size) console.log(`missing: ${[...missing].join(", ")}`);
} finally {
  fs.closeSync(fd);
}
