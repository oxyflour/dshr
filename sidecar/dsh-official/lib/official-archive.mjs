import { join } from 'node:path'

export function mountOfficialArchive(directory) {
  const vfs = process.getBuiltinModule('node:vfs')
  if (!vfs) throw new Error('official.zip requires Node with --experimental-vfs')
  const archive = process.getBuiltinModule('node:zlib').ZipFile.openSync(join(directory, 'official.zip'))
  const mount = vfs.create(new vfs.ZipProvider(archive))
  try {
    mount.mount()
  } catch (error) {
    archive.closeSync()
    throw error
  }
  return {
    directory: mount.mountPoint,
    url: `${mount.mountPointURL}/sidecar-dsh.mjs`,
    close() {
      mount.unmount()
      archive.closeSync()
    },
  }
}
