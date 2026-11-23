import { join } from 'node:path'
import { mountOfficialArchive } from './lib/official-archive.mjs'
import { registerModuleRoots } from './lib/module-roots.mjs'

const directory = import.meta.dirname
const mount = mountOfficialArchive(directory)
registerModuleRoots(mount.url, {
  native: join(directory, 'native'),
  user: process.env.DSHR_PLUGIN_ROOT,
  legacyDirectory: directory,
})
await import(mount.url)
