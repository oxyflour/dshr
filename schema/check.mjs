#!/usr/bin/env node
// Minimal JSON Schema (draft 2020-12 subset) validator for the M0 contract fixtures.
// Supported: $ref(local), type, const, enum, required, properties, additionalProperties,
// items, minItems, maxItems, minLength, maxLength, pattern, minimum, maximum, oneOf, allOf, anyOf.
// Replace with ajv once the JS toolchain is pinned; keep the fixture list as-is.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))

const schemas = {
  element: load('adc-v1/element.schema.json'),
  slotRegistration: load('adc-v1/slot-registration.schema.json'),
  manifest: load('cpp-v1/manifest.schema.json'),
  envelope: load('sidecar-rpc/envelope.schema.json'),
}

function load(rel) {
  return JSON.parse(readFileSync(join(here, rel), 'utf8'))
}

function kindOf(value) {
  if (Array.isArray(value)) return 'array'
  if (value === null) return 'null'
  if (Number.isInteger(value)) return 'integer'
  return typeof value
}

function matchesType(value, type) {
  const k = kindOf(value)
  if (type === 'number') return k === 'number' || k === 'integer'
  if (type === 'integer') return k === 'integer'
  return k === type
}

function resolveRef(root, ref) {
  if (!ref.startsWith('#/')) throw new Error(`unsupported $ref: ${ref}`)
  return ref
    .slice(2)
    .split('/')
    .reduce((node, part) => node[part], root)
}

function validate(schema, data, path, root) {
  const errors = []
  if (schema === true || schema === undefined) return errors
  if (schema.$ref) {
    return validate(resolveRef(root, schema.$ref), data, path, root)
  }

  if ('const' in schema && JSON.stringify(data) !== JSON.stringify(schema.const)) {
    errors.push(`${path}: expected const ${JSON.stringify(schema.const)}`)
  }
  if (schema.enum && !schema.enum.some((v) => JSON.stringify(v) === JSON.stringify(data))) {
    errors.push(`${path}: not in enum`)
  }
  if (schema.type) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type]
    if (!types.some((t) => matchesType(data, t))) {
      errors.push(`${path}: expected type ${types.join('|')}, got ${kindOf(data)}`)
      return errors
    }
  }

  if (kindOf(data) === 'object' || (typeof data === 'object' && data !== null && !Array.isArray(data))) {
    const isObject = typeof data === 'object' && data !== null && !Array.isArray(data)
    if (isObject) {
      for (const req of schema.required ?? []) {
        if (!(req in data)) errors.push(`${path}: missing required "${req}"`)
      }
      const props = schema.properties ?? {}
      for (const [key, value] of Object.entries(data)) {
        if (key in props) {
          errors.push(...validate(props[key], value, `${path}/${key}`, root))
        } else if (schema.additionalProperties === false) {
          errors.push(`${path}: unexpected property "${key}"`)
        } else if (schema.additionalProperties) {
          errors.push(...validate(schema.additionalProperties, value, `${path}/${key}`, root))
        }
      }
    }
  }

  if (Array.isArray(data)) {
    if (schema.minItems !== undefined && data.length < schema.minItems) {
      errors.push(`${path}: fewer than ${schema.minItems} items`)
    }
    if (schema.maxItems !== undefined && data.length > schema.maxItems) {
      errors.push(`${path}: more than ${schema.maxItems} items`)
    }
    if (schema.items) {
      data.forEach((item, i) => errors.push(...validate(schema.items, item, `${path}[${i}]`, root)))
    }
  }

  if (typeof data === 'string') {
    if (schema.minLength !== undefined && data.length < schema.minLength) {
      errors.push(`${path}: shorter than ${schema.minLength}`)
    }
    if (schema.maxLength !== undefined && data.length > schema.maxLength) {
      errors.push(`${path}: longer than ${schema.maxLength}`)
    }
    if (schema.pattern && !new RegExp(schema.pattern).test(data)) {
      errors.push(`${path}: does not match ${schema.pattern}`)
    }
  }

  if (typeof data === 'number') {
    if (schema.minimum !== undefined && data < schema.minimum) {
      errors.push(`${path}: below minimum ${schema.minimum}`)
    }
    if (schema.maximum !== undefined && data > schema.maximum) {
      errors.push(`${path}: above maximum ${schema.maximum}`)
    }
  }

  if (schema.allOf) {
    for (const sub of schema.allOf) errors.push(...validate(sub, data, path, root))
  }
  if (schema.anyOf && !schema.anyOf.some((s) => validate(s, data, path, root).length === 0)) {
    errors.push(`${path}: no anyOf branch matched`)
  }
  if (schema.oneOf) {
    const hits = schema.oneOf.filter((s) => validate(s, data, path, root).length === 0)
    if (hits.length !== 1) errors.push(`${path}: expected exactly one oneOf branch, got ${hits.length}`)
  }

  return errors
}

const cases = [
  // ---- ADC element tree ----
  ['element', 'valid: nested stack', {
    type: 'Stack',
    key: 'root',
    props: { gap: 8 },
    children: [
      { type: 'Text', props: { value: 'hello' } },
      { type: 'Button', props: { label: 'run', a11yLabel: 'run validation' } },
    ],
  }, true],
  ['element', 'invalid: unknown element', { type: 'WebView' }, false],
  ['element', 'invalid: missing type', { props: {} }, false],
  ['element', 'invalid: child unknown type', { type: 'Stack', children: [{ type: 'Canvas' }] }, false],
  ['element', 'invalid: unknown top-level field', { type: 'Text', style: {} }, false],

  // ---- ADC slot registration ----
  ['slotRegistration', 'valid: panel', {
    slot_id: 'workspace.panel',
    plugin_id: 'cad.validate',
    id: 'cad.inspector',
    order: 100,
    label: 'CAD Inspector',
    view: 'cad.inspector',
    location: 'right',
  }, true],
  ['slotRegistration', 'invalid: slot_id without dot', {
    slot_id: 'WorkspacePanel', plugin_id: 'cad.validate', id: 'x', view: 'v',
  }, false],
  ['slotRegistration', 'invalid: missing view', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'x',
  }, false],
  ['slotRegistration', 'invalid: plugin_id uppercase', {
    slot_id: 'workspace.panel', plugin_id: 'CAD.Validate', id: 'x', view: 'v',
  }, false],
  ['slotRegistration', 'invalid: bad location', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'x', view: 'v', location: 'top',
  }, false],

  // ---- CPP manifest ----
  ['manifest', 'valid: wasm tool plugin', {
    plugin: { id: 'cad.validate', version: '0.1.0', api: 'cordis-portable/v1', description: 'validate' },
    runtime: { kind: 'wasm-component', entry: 'cad_validate.wasm', fuel: 2000000000, max_memory_mb: 128, timeout_ms: 30000 },
    inject: { required: ['tool-registry/v1', 'ui-contributions/v1'], optional: ['storage/v1'] },
    permissions: { workspace_read: true, workspace_write: false, network: false },
    tools: [{
      name: 'cad_validate',
      description: 'Validate a STEP file',
      input_schema: 'schemas/cad_validate.input.json',
      output_schema: 'schemas/cad_validate.output.json',
      timeout_ms: 120000,
      cancelable: true,
      presentation: 'generic',
    }],
    ui: [{ slot: 'workspace.panel', id: 'cad.inspector', label: 'CAD Inspector', view: 'cad.inspector' }],
  }, true],
  ['manifest', 'invalid: api v2', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v2' },
    runtime: { kind: 'wasm-component' },
  }, false],
  ['manifest', 'invalid: unknown runtime kind', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'dll' },
  }, false],
  ['manifest', 'invalid: unknown top-level field', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'rust-static' },
    extra: true,
  }, false],
  ['manifest', 'invalid: memory over cap', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'wasm-component', max_memory_mb: 4096 },
  }, false],

  // ---- Sidecar envelope ----
  ['envelope', 'valid: tool.invoke', {
    version: 1,
    kind: 'tool.invoke',
    request_id: 'req-001',
    runtime_generation: 'node-0007',
    plugin_id: 'example.dsh.tool',
    registration_id: 'reg-42',
    session_id: 's-1',
    call_id: 'call-123',
    depth: 0,
    trace_id: 't-9f3c',
    ts: '2026-11-08T10:00:00.000Z',
    payload: { arguments: { path: 'part.step' } },
  }, true],
  ['envelope', 'invalid: missing trace_id', {
    version: 1, kind: 'tool.invoke', request_id: 'req-001', runtime_generation: 'node-0007',
    depth: 0, ts: '2026-11-08T10:00:00Z', payload: {},
  }, false],
  ['envelope', 'invalid: depth over limit', {
    version: 1, kind: 'tool.invoke', request_id: 'req-001', runtime_generation: 'node-0007',
    depth: 5, trace_id: 't', ts: '2026-11-08T10:00:00Z', payload: {},
  }, false],
  ['envelope', 'invalid: unknown kind', {
    version: 1, kind: 'tool.explode', request_id: 'req-001', runtime_generation: 'node-0007',
    depth: 0, trace_id: 't', ts: '2026-11-08T10:00:00Z', payload: {},
  }, false],
  ['envelope', 'invalid: bad error code', {
    version: 1, kind: 'tool.result', request_id: 'req-001', runtime_generation: 'node-0007',
    depth: 0, trace_id: 't', ts: '2026-11-08T10:00:00Z', payload: {},
    error: { code: 'timeout', message: 'slow' },
  }, false],
]

let failures = 0
for (const [schemaName, label, data, expectedValid] of cases) {
  const errors = validate(schemas[schemaName], data, '$', schemas[schemaName])
  const valid = errors.length === 0
  if (valid !== expectedValid) {
    failures += 1
    console.error(`FAIL [${schemaName}] ${label}`)
    console.error(`  expected ${expectedValid ? 'valid' : 'invalid'}, got ${valid ? 'valid' : 'invalid'}`)
    for (const e of errors) console.error(`  - ${e}`)
  } else {
    console.log(`ok   [${schemaName}] ${label}`)
  }
}

const total = cases.length
console.log(`\n${total - failures}/${total} cases passed`)
process.exit(failures === 0 ? 0 : 1)
