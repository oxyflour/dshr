import { existsSync } from 'node:fs'
import { createRequire, isBuiltin, registerHooks } from 'node:module'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

function packageName(specifier) {
  return /^(?:@[\w.-]+\/)?[\w-][\w.-]*(?=\/|$)/.exec(specifier)?.[0]
}

function root(directory) {
  if (!directory) return undefined
  const anchor = pathToFileURL(join(directory, 'package.json')).href
  const url = new URL('.', anchor).href
  return { directory, url, modules: new URL('node_modules/', url).href, anchor }
}

export function registerModuleRoots(officialEntry, { legacyDirectory, native, user } = {}) {
  const bundled = root(fileURLToPath(new URL('.', officialEntry)))
  const unpacked = root(native)
  const profile = root(user)
  const legacy = root(legacyDirectory)
  const official = [bundled, unpacked].filter(Boolean)
  const installed = [profile, legacy].filter(Boolean)
  if (!unpacked && !installed.length) return

  function lookup(name, roots) {
    return roots.find(({ directory }) => existsSync(join(directory, 'node_modules', name)))
  }

  registerHooks({
    resolve(specifier, context, nextResolve) {
      const name = !isBuiltin(specifier) && packageName(specifier)
      if (!name) return nextResolve(specifier, context)
      const parent = context.parentURL ?? ''
      const fromInstalled = installed.some(({ modules }) => parent.startsWith(modules))
      const fromUnpacked = unpacked && parent.startsWith(unpacked.modules)
      const fromBundled = parent.startsWith(bundled.url)
      const entry = parent === officialEntry
      const resolveFrom = ({ anchor }) => {
        // The CommonJS default resolver retains the original parent module internally.
        if (context.conditions.includes('require')) {
          const filename = createRequire(anchor).resolve(specifier)
          return { url: pathToFileURL(filename).href, shortCircuit: true }
        }
        return nextResolve(specifier, { ...context, parentURL: anchor })
      }

      if (fromInstalled && name === '@deepseek-ai/cordis') {
        const shared = lookup(name, official)
        if (shared) return resolveFrom(shared)
      }
      if (entry && unpacked) {
        const selected = lookup(name, [...official, ...installed])
        return selected ? resolveFrom(selected) : nextResolve(specifier, context)
      }
      if (entry && profile && existsSync(join(bundled.directory, 'node_modules', name, '.dshr-install.json'))) {
        if (lookup(name, [profile])) return resolveFrom(profile)
      }
      try {
        return nextResolve(specifier, context)
      } catch (error) {
        if (!['ERR_MODULE_NOT_FOUND', 'MODULE_NOT_FOUND'].includes(error.code)) throw error
        const fallback = entry ? lookup(name, installed)
          : fromInstalled || fromUnpacked ? lookup(name, official)
            : fromBundled && lookup(name, [unpacked].filter(Boolean))
        if (fallback) return resolveFrom(fallback)
        throw error
      }
    },
  })
}
