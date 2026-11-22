export const name = 'scoped-tool-test'
export const inject = ['tools']

export function apply(ctx) {
  const tools = ctx.tools
  const register = (label, scope) =>
    tools.register({
      name: 'scoped_echo',
      description: label,
      execute: () => ({ label }),
    }, scope)

  register('global')
  const sessionA = { sessionId: 's-1' }
  const disposeA = register('A', sessionA)
  register('B', { sessionId: 's-2' })
  tools.register({
    name: 'replace_scoped_echo',
    execute: () => {
      const before = tools.get('scoped_echo', sessionA).description
      let duplicateRejected = false
      try {
        register('duplicate', sessionA)
      } catch {
        duplicateRejected = true
      }
      disposeA()
      register('A replacement', sessionA)
      disposeA()
      register('transient', { sessionId: 's-3' })()
      return { before, duplicateRejected }
    },
  })
}
