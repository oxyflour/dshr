import { createInterface } from 'node:readline'

const input = createInterface({ input: process.stdin })
let generation
input.on('line', (line) => {
  const msg = JSON.parse(line)
  const send = (kind, requestId, payload, extra = {}) => {
    process.stdout.write(JSON.stringify({
      version: 1,
      kind,
      request_id: requestId,
      runtime_generation: generation,
      depth: 0,
      trace_id: msg.trace_id,
      ts: new Date().toISOString(),
      payload,
      ...extra,
    }) + '\n')
  }
  if (msg.kind === 'runtime.hello') {
    generation = msg.payload.generation
    send('runtime.ready', msg.request_id, {
      protocol: 1,
      generation,
      dsh_version: 'headless-fixture',
      sidecar_caps: ['tool.cancel'],
    })
    for (const name of ['FIXTURE_TOOL', 'probe_fixture']) {
      send('tool.register', `reg-${name}`, {
        name,
        description: name,
        input_schema: { type: 'object' },
        presentation: 'FIXTURE_PRESENTATION',
      }, { plugin_id: 'headless.fixture' })
    }
  } else if (msg.kind === 'tool.invoke') {
    send('tool.result', msg.request_id, {
      call_id: msg.call_id,
      is_error: false,
      content: [{ type: 'text', text: 'fixture result' }],
    }, { call_id: msg.call_id })
  }
})
