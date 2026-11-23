import { copyFileSync, cpSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { ZipBuffer } from 'node:zlib'

const source = resolve(import.meta.dirname, '../sidecar/dsh-official')
const [modules, destination] = process.argv.slice(2).map(path => resolve(path))
if (!modules || !destination) throw new Error('usage: node tools/archive-official-sidecar.mjs <node_modules> <sidecar>')

function packagesIn(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    if (!entry.isDirectory() || entry.name.startsWith('.')) return []
    if (!entry.name.startsWith('@')) return [entry.name]
    return readdirSync(join(directory, entry.name), { withFileTypes: true })
      .filter(child => child.isDirectory())
      .map(child => `${entry.name}/${child.name}`)
  }).sort()
}

function filesIn(directory) {
  return readdirSync(directory, { withFileTypes: true, recursive: true })
    .filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath, entry.name))
    .sort()
}

const packages = new Map(packagesIn(modules).map(name => [name, filesIn(join(modules, name))]))
const native = new Set(['@deepseek-ai/dsh-ptc-runtime-node'])
for (const [name, files] of packages) {
  if (files.some(path => /\.(dll|exe|node)$/i.test(path))) native.add(name)
}
// The subprocess runner and Office workers use Node resolution in separate processes or isolates.
const childDependencies = new Set([
  '@deepseek-ai/dsh-subprocess-local',
  '@deepseek-ai/dsh-sandbox-windows-acl',
  '@deepseek-ai/libreoffice-kit',
])
for (const name of childDependencies) {
  if (!packages.has(name)) continue
  native.add(name)
  const manifest = JSON.parse(readFileSync(join(modules, name, 'package.json'), 'utf8'))
  for (const dependencies of [manifest.dependencies, manifest.optionalDependencies, manifest.peerDependencies]) {
    for (const dependency of Object.keys(dependencies ?? {})) {
      if (packages.has(dependency)) childDependencies.add(dependency)
    }
  }
}

const emptyZip = Buffer.alloc(22)
emptyZip.writeUInt32LE(0x06054b50)
const zip = new ZipBuffer(emptyZip)
const mtime = new Date('2000-01-01T00:00:00Z')
function archiveFile(base, path, prefix = '') {
  const name = relative(base, path).split(sep).join('/')
  zip.addSync(prefix + name, readFileSync(path), { mtime })
}

mkdirSync(join(destination, 'lib'), { recursive: true })
archiveFile(source, join(source, 'sidecar-dsh.mjs'))
for (const path of filesIn(join(source, 'lib'))) archiveFile(source, path)
for (const [name, files] of packages) {
  if (native.has(name)) {
    const target = join(destination, 'native', 'node_modules', name)
    mkdirSync(dirname(target), { recursive: true })
    cpSync(join(modules, name), target, { recursive: true })
  } else {
    for (const path of files) archiveFile(modules, path, 'node_modules/')
  }
}
writeFileSync(join(destination, 'official.zip'), zip.toBufferSync(), { flag: 'wx' })
copyFileSync(join(source, 'launch-archive.mjs'), join(destination, 'sidecar-dsh.mjs'))
for (const name of ['official-archive.mjs', 'module-roots.mjs']) {
  copyFileSync(join(source, 'lib', name), join(destination, 'lib', name))
}
console.log(`[archive-official-sidecar] ${zip.size} VFS entries; ${[...native].filter(name => packages.has(name)).length} unpacked packages`)
