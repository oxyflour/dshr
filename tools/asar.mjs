// 只读查看安装包里的 app.asar（DeepSeek Harness 桌面版）。
//
// 用法：
//   node tools/asar.mjs ls <路径>            # 列直接子项
//   node tools/asar.mjs list <路径>          # 递归列文件
//   node tools/asar.mjs cat <文件路径>       # 输出文件内容
//   node tools/asar.mjs extract <路径> <目录> # 抽取到目录（保留相对路径）
//
// 缺省 asar 路径可通过 DSHR_ASAR 覆盖。

import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { lookup, openAsar, readFile, walk } from "./lib/asar.mjs";

const ASAR =
  process.env.DSHR_ASAR ||
  path.join(os.homedir(), "AppData/Local/Programs/DeepSeek Harness/resources/app.asar");

const [cmd, a, b] = process.argv.slice(2);
const { fd, header, base } = openAsar(ASAR);
try {
  if (cmd === "ls") {
    const node = a ? lookup(header, a) : header;
    if (!node) throw new Error(`not found: ${a}`);
    for (const [key, value] of Object.entries(node.files || {})) {
      console.log((value.files ? "dir  " : "file ") + key);
    }
  } else if (cmd === "list") {
    const node = a ? lookup(header, a) : header;
    if (!node) throw new Error(`not found: ${a}`);
    const files = [];
    walk(node, a || "", files);
    for (const file of files.slice(0, 200)) console.log(file);
  } else if (cmd === "cat") {
    const node = lookup(header, a);
    if (!node) throw new Error(`not found: ${a}`);
    process.stdout.write(readFile(fd, base, node).toString("utf8"));
  } else if (cmd === "extract") {
    const node = lookup(header, a);
    if (!node) throw new Error(`not found: ${a}`);
    const files = [];
    walk(node, a, files);
    for (const file of files) {
      const entry = lookup(header, file);
      if (!entry || entry.link !== undefined || entry.size === undefined) continue;
      const dest = path.join(b, file.slice(a.length));
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      if (entry.unpacked) {
        const unpacked = path.join(path.dirname(ASAR), "app.asar.unpacked", file);
        if (fs.existsSync(unpacked)) fs.copyFileSync(unpacked, dest);
        continue;
      }
      fs.writeFileSync(dest, readFile(fd, base, entry));
    }
    console.log(`extracted ${files.length} files to ${b}`);
  } else {
    console.error("usage: node tools/asar.mjs <ls|list|cat|extract> ...");
    process.exit(2);
  }
} finally {
  fs.closeSync(fd);
}
