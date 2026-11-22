const MAX_CONTEXT_BYTES = 64 * 1024

function ownedText(message) {
  return message.content.filter(block => block.type === 'text')
    .map(block => block.text).join('\n')
}

export async function collectAgentStep({ contexts, agent, signal, step, userText, modelSelection,
  toolSet, bindings, jobNotices = [], reportError }) {
  const unavailable = new Set()
  const hookContext = { agent, signal, step, modelSelection, toolSet }
  const messages = typeof userText === 'string' && userText.length > 0
    ? [{
        id: 'current-user',
        source: { kind: 'user' },
        content: [{ type: 'text', text: userText }],
      }]
    : []
  const params = { ...hookContext, messages }
  const run = index => index < contexts.length
    ? contexts[index].waterfall('agent/pre-step', params, () => run(index + 1))
    : { kind: 'enter', messages }
  const decision = await run(0)
  signal.throwIfAborted()

  if (decision.kind === 'enter') {
    for (const message of decision.messages) {
      if (message.source?.kind !== 'skill-invocation') continue
      for (const context of contexts) {
        await context.parallel('session/event', agent.session, { type: 'user/message', data: message })
      }
    }
  }

  for (const [name, binding] of Object.entries(bindings)) {
    if (!binding?.when) continue
    try {
      if (!await binding.when(hookContext)) unavailable.add(name)
    } catch (error) {
      reportError(name, error)
      unavailable.add(name)
    }
  }
  for (const name of unavailable) delete bindings[name]

  const system = []
  const instructions = []
  let remaining = MAX_CONTEXT_BYTES
  const push = (target, text) => {
    if (typeof text !== 'string' || !text.trim()) return
    const bytes = Buffer.byteLength(text)
    if (bytes > remaining) return
    target.push(text)
    remaining -= bytes
  }
  const promptParts = contexts.flatMap(context => context.get('systemPrompt').sections())
    .sort((a, b) => (a.order ?? 100) - (b.order ?? 100))
  for (const part of promptParts) {
    const text = typeof part.text === 'function'
      ? await part.text({ ...hookContext, scope: agent })
      : part.text
    push(system, text)
  }
  for (const notice of jobNotices) push(system, ownedText(notice))
  if (decision.kind === 'enter') {
    for (const message of decision.messages) {
      if (message.source?.kind === 'skill-catalog') push(system, ownedText(message))
      if (message.source?.kind === 'skill-invocation') push(instructions, ownedText(message))
    }
  }
  return {
    unavailable_tools: [...unavailable],
    system_sections: system,
    turn_instructions: instructions,
  }
}
