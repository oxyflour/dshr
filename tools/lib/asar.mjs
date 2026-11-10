// 只读 asar 解析：DeepSeek Harness 的桌面版把 JS 打包在 app.asar 里。
// 用途：tools/asar.mjs（人工查看/抽取）与 tools/dsh-vendor.mjs（抽取官方插件闭包）共用。

import fs from "node:fs";

export function openAsar(asarPath) {
  const fd = fs.openSync(asarPath, "r");
  const b = Buffer.alloc(16);
  fs.readSync(fd, b, 0, 16, 0);
  const headSize = b.readUInt32LE(4);
  const hb = Buffer.alloc(headSize);
  fs.readSync(fd, hb, 0, headSize, 8);
  const hstr = hb.toString("utf8");
  const header = JSON.parse(hstr.slice(hstr.indexOf("{"), hstr.lastIndexOf("}") + 1));
  return { fd, header, base: 8 + headSize };
}

export function lookup(header, path) {
  let node = header;
  for (const part of path.split("/").filter(Boolean)) {
    if (!node.files || !node.files[part]) return undefined;
    node = node.files[part];
  }
  return node;
}

export function readFile(fd, base, node) {
  const buf = Buffer.alloc(node.size);
  fs.readSync(fd, buf, 0, node.size, base + parseInt(node.offset, 10));
  return buf;
}

/** 列出某个目录节点下的全部文件（相对 asar 根）。 */
export function walk(node, prefix, out) {
  if (!node.files) {
    out.push(prefix);
    return;
  }
  for (const [key, value] of Object.entries(node.files)) walk(value, prefix + "/" + key, out);
}
