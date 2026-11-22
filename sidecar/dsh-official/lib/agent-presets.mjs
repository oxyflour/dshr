export function createAgentPresets(mount) {
  const current = new Map()
  const sessions = new Map()
  let nextRevision = 0

  async function collect(revision) {
    if (revision && revision.retired && revision.users === 0) {
      await revision.dispose?.()
    }
  }

  return {
    async sync(definitions) {
      if (!definitions || Array.isArray(definitions) || typeof definitions !== 'object') {
        throw new Error('preset definitions must be an object')
      }
      for (const [id, entries] of Object.entries(definitions)) {
        if (!id || !Array.isArray(entries)) throw new Error('invalid preset declaration')
      }
      for (const [id, entries] of Object.entries(definitions)) {
        const signature = JSON.stringify(entries)
        const previous = current.get(id)
        if (previous?.signature === signature) continue
        const revision = { id, key: `preset-${++nextRevision}`, signature, users: 0, retired: false }
        try {
          revision.dispose = await mount(revision.key, entries)
        } catch (error) {
          revision.error = String(error?.message ?? error)
        }
        current.set(id, revision)
        if (previous) {
          previous.retired = true
          await collect(previous)
        }
      }
      for (const [id, revision] of current) {
        if (Object.hasOwn(definitions, id)) continue
        current.delete(id)
        revision.retired = true
        await collect(revision)
      }
      return Object.fromEntries([...current].map(([id, revision]) => [id, {
        revision: revision.key, ...(revision.error && { error: revision.error }),
      }]))
    },
    bind(session, { preset, parent_session_id }) {
      if (!session) throw new Error('invalid session for preset binding')
      const bound = sessions.get(session)
      if (bound) {
        if (preset && bound.id !== preset) throw new Error('session already uses a different preset')
        return bound
      }
      const revision = parent_session_id ? sessions.get(parent_session_id) : current.get(preset)
      if (!revision) throw new Error(parent_session_id ? 'parent has no preset binding' : `unknown preset ${preset}`)
      if (preset && preset !== revision.id) throw new Error('parent preset identity does not match')
      if (revision.error) throw new Error(`preset ${revision.id}: ${revision.error}`)
      revision.users += 1
      sessions.set(session, revision)
      return revision
    },
    binding(session) {
      return sessions.get(session)?.key
    },
    async release(session) {
      const revision = sessions.get(session)
      if (!revision) return
      sessions.delete(session)
      revision.users -= 1
      await collect(revision)
    },
  }
}
