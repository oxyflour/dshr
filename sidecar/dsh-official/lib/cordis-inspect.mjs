export const INSPECT_PLUGIN = '@deepseek-ai/dsh-tool-cordis'
export const INSPECT_TOOLS = ['cordis_inspect_list', 'cordis_inspect_query']

export async function mountCordisInspection(root, hostServices) {
  const { CordisInspectRegistryService } = await import('@deepseek-ai/dsh-cordis-host-runner')
  await root.plugin(CordisInspectRegistryService, 3_000)
  const registry = root.get('cordisInspect')
  const provider = ({ id, description, name, query }) => ({
    manifest: {
      id, description,
      methods: [{
        name, description,
        inputSchema: { type: 'object', properties: {}, additionalProperties: false },
        outputSchema: { type: 'object' },
      }],
    },
    query,
  })
  const tools = provider({
    id: 'Tool', name: 'listTools', description: 'Tool schemas available to the calling session.',
    query: (_method, _input, { agent }) => ({ tools: root.get('tools').schemas(agent) }),
  })
  const services = provider({
    id: 'HostService', name: 'listServices', description: 'Metadata for negotiated host services.',
    query: () => ({ services: hostServices.catalog }),
  })
  for (const entry of [tools, services]) {
    root.effect(() => registry.register(entry), `inspect ${entry.manifest.id}`)
  }
}
