#!/usr/bin/env node
// Minimal JSON Schema (draft 2020-12 subset) validator for the M0 contract fixtures.
// Supported: $ref(local), type, const, enum, required, properties, additionalProperties,
// items, minItems, maxItems, minLength, maxLength, pattern, minimum, maximum, oneOf, allOf, anyOf.
// Replace with ajv once the JS toolchain is pinned; keep the fixture list as a stable regression baseline.
//
// ADC-v1 冻结工件（M3）在此文件里多两层校验：
//   slots            → schema/adc-v1/slots.v1.json 的目录自检（12 slot / kind / scope / 契约 / key 域 / contribution）
//   registrationContract → 注册载荷先过 slot-registration.schema.json（kind 级形状），再按 slot_id 查目录做字段级契约校验
// 两层都对齐 docs/spec/adc-v1.md §5；未知元素在作者期 lint 仍判 invalid（运行时降级另做，见 spec §11）。

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))

const schemas = {
  element: load('adc-v1/element.schema.json'),
  slotRegistration: load('adc-v1/slot-registration.schema.json'),
  manifest: load('cpp-v1/manifest.schema.json'),
  trust: load('cpp-v1/trust.schema.json'),
  envelope: load('sidecar-rpc/envelope.schema.json'),
}

const catalog = load('adc-v1/slots.v1.json')

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

// ---------------------------------------------------------------------------
// ADC-v1 slot 目录自检：slots.v1.json 是 spec §5 的机器可读镜像（冻结基线）。
// 新增/删除 slot 属 minor 变更：先改 docs/spec/adc-v1.md §5 与本表，再更新这里。
// ---------------------------------------------------------------------------

const FROZEN_SLOT_IDS = [
  'workspace.panel',
  'viewer.tab',
  'settings.section',
  'settings.general.item',
  'command',
  'menu.item',
  'conversation.chat.node',
  'conversation.input.left',
  'conversation.input.right',
  'tool.call.toolview',
  'tool.result.extra',
  'status.bar.item',
]
const KINDS = ['single', 'list', 'keyed', 'chain']
const SCOPES = ['root', 'session', 'session-maybe']
const FIELD_KINDS = new Set(['string', 'number', 'boolean', 'enum', 'string[]'])
const SLOT_FIELDS = new Set([
  'slot_id', 'kind', 'scope', 'exclusive', 'replacement_risk', 'contribution', 'registration', 'key_domain', 'note',
])
// `command` 是 spec §5 保留的历史短名（无点分前缀），其余 slot_id 均为点分小写。
const SLOT_ID_RE = /^[a-z0-9]+(\.[a-z0-9-]+)*$/

function sameSet(a, b) {
  if (a.length !== b.length) return false
  const set = new Set(b)
  return a.every((v) => set.has(v))
}

function checkPrimaryField(name, spec) {
  const errors = []
  const p = `field_types/${name}`
  if (!spec || typeof spec !== 'object') return [`${p}: must be an object`]
  if (!FIELD_KINDS.has(spec.kind)) return [`${p}: unsupported kind "${spec.kind}"`]
  if (spec.kind === 'enum' && !(Array.isArray(spec.values) && spec.values.length > 0)) {
    errors.push(`${p}: enum values must be a non-empty array`)
  }
  if (spec.kind === 'string[]') {
    const pattern = spec.items?.pattern
    if (pattern === undefined) errors.push(`${p}: string[] must declare items.pattern`)
    else {
      try { new RegExp(pattern) } catch { errors.push(`${p}: invalid items.pattern`) }
    }
  }
  if (spec.kind === 'string' && spec.pattern !== undefined) {
    try { new RegExp(spec.pattern) } catch { errors.push(`${p}: invalid pattern`) }
  }
  return errors
}

function checkKeyDomain(slot, path) {
  const errors = []
  const kd = slot.key_domain
  if (!kd || typeof kd !== 'object' || Array.isArray(kd)) {
    return [`${path}: key_domain must be an object`]
  }
  if (kd.mode === 'open') {
    if ('source' in kd || 'values' in kd) errors.push(`${path}: open key_domain must not declare source/values`)
  } else if (kd.mode === 'closed') {
    const hasValues = Array.isArray(kd.values) && kd.values.length > 0
    if (kd.source === 'host') {
      if (hasValues) errors.push(`${path}: closed key_domain with source="host" must leave values empty`)
    } else if (!hasValues) {
      errors.push(`${path}: closed key_domain must declare non-empty values or source="host"`)
    }
  } else {
    errors.push(`${path}: key_domain.mode must be "open" or "closed"`)
  }
  return errors
}

function checkSlotsCatalog(cat) {
  const errors = []
  if (!cat || typeof cat !== 'object' || Array.isArray(cat)) return ['$: catalog must be an object']
  if (cat.catalog !== 'adc.slots/v1') errors.push('$: catalog must be "adc.slots/v1"')
  if (cat.version !== 1) errors.push('$: version must be 1')
  if (cat.status !== 'Review') errors.push('$: status must be "Review"')

  if (!sameSet(Object.keys(cat.kinds ?? {}), KINDS)) errors.push(`$: kinds must be exactly ${KINDS.join('/')}`)
  if (!sameSet(Object.keys(cat.scopes ?? {}), SCOPES)) errors.push(`$: scopes must be exactly ${SCOPES.join('/')}`)

  const fieldTypes = cat.field_types ?? {}
  for (const [name, spec] of Object.entries(fieldTypes)) errors.push(...checkPrimaryField(name, spec))

  const contributions = cat.contributions ?? {}
  for (const [name, contribution] of Object.entries(contributions)) {
    if (!Array.isArray(contribution.slots) || contribution.slots.length === 0) {
      errors.push(`contributions/${name}: slots must be a non-empty array`)
    }
  }

  const slots = cat.slots
  if (!Array.isArray(slots)) return errors.concat('$: slots must be an array')
  if (slots.length !== 12) errors.push(`$: slots must contain 12 entries, got ${slots.length}`)
  if (!sameSet(slots.map((s) => s.slot_id), FROZEN_SLOT_IDS)) {
    errors.push('$: slot_id set must match frozen ADC-v1 list (see docs/spec/adc-v1.md §5)')
  }

  const seenIds = new Set()
  const byContribution = {}
  for (const [i, slot] of slots.entries()) {
    const p = `slots[${i}]`
    if (!slot || typeof slot !== 'object' || Array.isArray(slot)) {
      errors.push(`${p}: must be an object`)
      continue
    }
    for (const key of Object.keys(slot)) {
      if (!SLOT_FIELDS.has(key)) errors.push(`${p}: unknown field "${key}"`)
    }
    if (!SLOT_ID_RE.test(slot.slot_id)) errors.push(`${p}: slot_id does not match ${SLOT_ID_RE}`)
    if (seenIds.has(slot.slot_id)) errors.push(`${p}: duplicate slot_id "${slot.slot_id}"`)
    seenIds.add(slot.slot_id)
    if (!KINDS.includes(slot.kind)) errors.push(`${p}: unknown kind "${slot.kind}"`)
    if (!SCOPES.includes(slot.scope)) errors.push(`${p}: unknown scope "${slot.scope}"`)
    if (typeof slot.exclusive !== 'boolean') errors.push(`${p}: exclusive must be boolean`)
    if (!(cat.replacement_risks ?? []).includes(slot.replacement_risk)) {
      errors.push(`${p}: unknown replacement_risk "${slot.replacement_risk}"`)
    }
    if (!(slot.contribution in contributions)) {
      errors.push(`${p}: contribution "${slot.contribution}" is not declared in contributions`)
    } else {
      (byContribution[slot.contribution] ??= []).push(slot.slot_id)
    }

    const registration = slot.registration
    if (!registration || typeof registration !== 'object' || Array.isArray(registration)) {
      errors.push(`${p}: registration must be an object`)
      continue
    }
    for (const key of Object.keys(registration)) {
      if (key !== 'required' && key !== 'optional') errors.push(`${p}: registration has unknown field "${key}"`)
    }
    const required = registration.required
    const optional = registration.optional
    if (!Array.isArray(required) || required.length === 0) {
      errors.push(`${p}: registration.required must be a non-empty array`)
      continue
    }
    if (!Array.isArray(optional)) errors.push(`${p}: registration.optional must be an array`)
    const requiredList = required.slice()
    const optionalList = Array.isArray(optional) ? optional.slice() : []
    if (new Set(requiredList).size !== requiredList.length) errors.push(`${p}: duplicate field in registration.required`)
    if (new Set(optionalList).size !== optionalList.length) errors.push(`${p}: duplicate field in registration.optional`)
    for (const field of requiredList) {
      if (!(field in fieldTypes)) errors.push(`${p}: required field "${field}" is not declared in field_types`)
      if (optionalList.includes(field)) errors.push(`${p}: field "${field}" is both required and optional`)
    }
    for (const field of optionalList) {
      if (!(field in fieldTypes)) errors.push(`${p}: optional field "${field}" is not declared in field_types`)
    }

    if (slot.kind === 'keyed') {
      if (!requiredList.includes('key')) errors.push(`${p}: keyed slot must require "key"`)
      if (!('key_domain' in slot)) errors.push(`${p}: keyed slot must declare key_domain`)
      else errors.push(...checkKeyDomain(slot, `${p}/key_domain`))
    } else {
      if (requiredList.includes('key') || optionalList.includes('key')) {
        errors.push(`${p}: ${slot.kind} slot must not declare "key"`)
      }
      if ('key_domain' in slot) errors.push(`${p}: non-keyed slot must not declare key_domain`)
      if (slot.kind === 'list' && !requiredList.includes('id')) errors.push(`${p}: list slot must require "id"`)
    }
  }

  for (const [name, contribution] of Object.entries(contributions)) {
    if (Array.isArray(contribution.slots) && contribution.slots.length > 0) {
      if (!sameSet(contribution.slots, byContribution[name] ?? [])) {
        errors.push(`contributions/${name}: slots list must equal the slots assigned to this contribution`)
      }
    }
  }

  return errors
}

// 注册契约校验：先把 kind 级形状交给 slot-registration.schema.json，再按 slot_id 查目录做字段级校验。
function checkRegistration(payload) {
  const schemaErrors = validate(schemas.slotRegistration, payload, '$', schemas.slotRegistration)
  if (schemaErrors.length > 0) return schemaErrors
  const errors = []
  const slot = catalog.slots.find((s) => s.slot_id === payload.slot_id)
  if (!slot) return [`$: unknown slot_id "${payload.slot_id}"（不在 slots.v1.json 目录内）`]
  const allowed = new Set([...slot.registration.required, ...slot.registration.optional])
  for (const key of Object.keys(payload)) {
    if (key === 'slot_id' || key === 'plugin_id') continue
    if (!allowed.has(key)) errors.push(`$: unexpected field "${key}" for slot ${slot.slot_id}`)
  }
  for (const field of slot.registration.required) {
    if (!(field in payload)) errors.push(`$: missing required "${field}" for slot ${slot.slot_id}`)
  }
  for (const field of [...slot.registration.required, ...slot.registration.optional]) {
    if (!(field in payload)) continue
    errors.push(...checkField(field, catalog.field_types[field], payload[field]))
  }
  const keyDomain = slot.key_domain
  if (keyDomain?.mode === 'closed' && Array.isArray(keyDomain.values) && keyDomain.values.length > 0) {
    if (!keyDomain.values.includes(payload.key)) {
      errors.push(`$: key "${payload.key}" is not in the closed key domain of ${slot.slot_id}`)
    }
  }
  return errors
}

function checkField(name, spec, value) {
  const path = `$/${name}`
  const errors = []
  if (!spec) return [`${path}: no field_type declared`]
  switch (spec.kind) {
    case 'string':
      if (typeof value !== 'string') return [`${path}: expected string`]
      if (spec.minLength !== undefined && value.length < spec.minLength) {
        errors.push(`${path}: shorter than ${spec.minLength}`)
      }
      if (spec.maxLength !== undefined && value.length > spec.maxLength) {
        errors.push(`${path}: longer than ${spec.maxLength}`)
      }
      if (spec.pattern && !new RegExp(spec.pattern).test(value)) errors.push(`${path}: does not match ${spec.pattern}`)
      break
    case 'number':
      if (typeof value !== 'number' || !Number.isFinite(value)) return [`${path}: expected finite number`]
      if (spec.minimum !== undefined && value < spec.minimum) errors.push(`${path}: below minimum ${spec.minimum}`)
      if (spec.maximum !== undefined && value > spec.maximum) errors.push(`${path}: above maximum ${spec.maximum}`)
      break
    case 'boolean':
      if (typeof value !== 'boolean') errors.push(`${path}: expected boolean`)
      break
    case 'enum':
      if (!spec.values.includes(value)) errors.push(`${path}: not in enum`)
      break
    case 'string[]':
      if (!Array.isArray(value)) return [`${path}: expected string[]`]
      if (spec.maxItems !== undefined && value.length > spec.maxItems) errors.push(`${path}: more than ${spec.maxItems} items`)
      for (const [i, item] of value.entries()) {
        if (typeof item !== 'string') errors.push(`${path}[${i}]: expected string`)
        else if (spec.items?.pattern && !new RegExp(spec.items.pattern).test(item)) {
          errors.push(`${path}[${i}]: does not match ${spec.items.pattern}`)
        }
      }
      break
    default:
      errors.push(`${path}: unsupported field kind "${spec.kind}"`)
  }
  return errors
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const cloneCatalog = () => JSON.parse(JSON.stringify(catalog))

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

  // ---- ADC element props（M3 冻结补充：字段级约束） ----
  ['element', 'valid: Grid with span child', {
    type: 'Grid',
    props: { columns: 12, gap: 4 },
    children: [{ type: 'Text', props: { value: 'a', span: 6 } }],
  }, true],
  ['element', 'valid: Button with action', {
    type: 'Button',
    props: { label: 'run', a11yLabel: 'run validation', disabled: false, onPress: { call: 'cad.validate', args: { path: 'part.step' } } },
  }, true],
  ['element', 'valid: Select with options', {
    type: 'Select',
    props: { value: 'a', options: [{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }], onChange: { command: 'cad.select' } },
  }, true],
  ['element', 'valid: Table with rows', {
    type: 'Table',
    props: { columns: [{ key: 'name', title: 'Name', width: 120 }], rows: [{ name: 'part.step' }] },
  }, true],
  ['element', 'valid: Chart with 8 points', {
    type: 'Chart',
    props: {
      kind: 'bars',
      data: Array.from({ length: 8 }, (_, i) => ({ label: `p${i}`, value: i })),
    },
  }, true],
  ['element', 'valid: Card with children', {
    type: 'Card',
    props: { title: 'CAD' },
    children: [{ type: 'Text', props: { value: 'hello' } }],
  }, true],
  ['element', 'valid: Tabs with active-tab children', {
    type: 'Tabs',
    props: { tabs: [{ key: 'a', label: 'A' }], activeKey: 'a' },
    children: [{ type: 'Text', props: { value: 'tab content' } }],
  }, true],
  ['element', 'valid: Image attachment ref', {
    type: 'Image',
    props: { attachmentId: 'sha256-abcdef', alt: 'diagram', fit: 'contain' },
  }, true],
  ['element', 'invalid: Grid columns over 12', { type: 'Grid', props: { columns: 13 } }, false],
  ['element', 'invalid: Stack gap outside scale', { type: 'Stack', props: { gap: 5 } }, false],
  ['element', 'invalid: Button missing label', { type: 'Button', props: { a11yLabel: 'run' } }, false],
  ['element', 'invalid: Select missing options', { type: 'Select', props: { value: 'a' } }, false],
  ['element', 'invalid: Chart unknown kind', { type: 'Chart', props: { kind: 'pie', data: [{ label: 'a', value: 1 }] } }, false],
  ['element', 'invalid: Image missing attachmentId', { type: 'Image', props: { alt: 'x' } }, false],
  ['element', 'invalid: leaf with children', { type: 'Divider', children: [{ type: 'Text', props: { value: 'x' } }] }, false],
  ['element', 'invalid: Text unknown prop', { type: 'Text', props: { value: 'x', bogus: true } }, false],
  ['element', 'invalid: openExternal non-https scheme', {
    type: 'Button',
    props: { label: 'open', onPress: { openExternal: 'file:///etc/passwd' } },
  }, false],
  // 作者期 schema 故意不把 a11yLabel 设为必填（否则旧 fixture 的 Button 会失败）；运行时 MUST 拒绝缺 a11yLabel 的交互元素。
  ['element', 'valid(schema) / rejected(runtime): Button 缺 a11yLabel —— ADC-ELEM-A11Y-001', {
    type: 'Button',
    props: { label: 'run' },
  }, true],

  // ---- ADC slot registration（kind 级形状） ----
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
  ['slotRegistration', 'valid: keyed viewer.tab', {
    slot_id: 'viewer.tab', plugin_id: 'cad.viewer', key: 'step', view: 'cad.step-viewer',
  }, true],
  ['slotRegistration', 'valid: keyed menu.item with id/label/run', {
    slot_id: 'menu.item', plugin_id: 'cad.menu', key: 'tools', id: 'cad.menu.item', order: 10,
    label: 'CAD', run: 'cad.open',
  }, true],
  ['slotRegistration', 'invalid: keyed missing key', {
    slot_id: 'tool.call.toolview', plugin_id: 'cad.validate', view: 'v',
  }, false],
  ['slotRegistration', 'invalid: unkeyed carries key', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'x', view: 'v', key: 'k',
  }, false],
  ['slotRegistration', 'invalid: keyed carries location', {
    slot_id: 'tool.call.toolview', plugin_id: 'cad.validate', key: 'cad_validate', view: 'v', location: 'right',
  }, false],
  ['slotRegistration', 'invalid: order type string', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'x', view: 'v', order: '100',
  }, false],

  // ---- ADC slot registration contract（查 slots.v1.json 的字段级契约） ----
  ['registrationContract', 'valid: workspace.panel', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'cad.inspector',
    label: 'CAD Inspector', location: 'right', view: 'cad.inspector',
  }, true],
  ['registrationContract', 'invalid: workspace.panel missing label', {
    slot_id: 'workspace.panel', plugin_id: 'cad.validate', id: 'cad.inspector',
    location: 'right', view: 'cad.inspector',
  }, false],
  ['registrationContract', 'invalid: settings.section extra location field', {
    slot_id: 'settings.section', plugin_id: 'cad.settings', id: 'x',
    label: 'X', location: 'left', view: 'v',
  }, false],
  ['registrationContract', 'valid: command with keys', {
    slot_id: 'command', plugin_id: 'cad.validate', id: 'cad.validate.run',
    label: 'Validate', run: 'cad.validate', keys: ['Ctrl+Shift+V'],
  }, true],
  ['registrationContract', 'invalid: command missing run', {
    slot_id: 'command', plugin_id: 'cad.validate', id: 'cad.validate.run', label: 'Validate',
  }, false],
  ['registrationContract', 'valid: keyed tool.call.toolview', {
    slot_id: 'tool.call.toolview', plugin_id: 'cad.validate', key: 'cad_validate', view: 'cad.validation-result',
  }, true],
  ['registrationContract', 'invalid: unknown slot_id', {
    slot_id: 'ui.newthing', plugin_id: 'cad.validate', id: 'x', view: 'v',
  }, false],
  ['registrationContract', 'invalid: menu.item missing id', {
    slot_id: 'menu.item', plugin_id: 'cad.menu', key: 'tools', label: 'CAD', run: 'cad.open',
  }, false],

  // ---- ADC slots.v1.json 目录自检 ----
  ['slots', 'valid: 目录自检（12 slot / kind / scope / 契约 / key 域 / contribution）', catalog, true],
  ['slots', 'invalid: keyed slot 缺 key_domain', (() => {
    const copy = cloneCatalog()
    delete copy.slots.find((s) => s.slot_id === 'conversation.chat.node').key_domain
    return copy
  })(), false],
  ['slots', 'invalid: 重复 slot_id', (() => {
    const copy = cloneCatalog()
    copy.slots.find((s) => s.slot_id === 'status.bar.item').slot_id = 'workspace.panel'
    return copy
  })(), false],
  ['slots', 'invalid: 契约字段未在 field_types 声明', (() => {
    const copy = cloneCatalog()
    copy.slots.find((s) => s.slot_id === 'command').registration.required.push('bogus_field')
    return copy
  })(), false],
  ['slots', 'invalid: contribution 未登记', (() => {
    const copy = cloneCatalog()
    copy.slots.find((s) => s.slot_id === 'status.bar.item').contribution = 'unknown_group'
    return copy
  })(), false],

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

  // ---- dshr-trust/v1 包级信任文件（M3 G2） ----
  ['trust', 'valid: unsigned package trust file', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: { digest_sha256: 'a'.repeat(64) },
  }, true],
  ['trust', 'valid: ed25519 signed trust file', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: { digest_sha256: 'a'.repeat(64) },
    signature: { scheme: 'ed25519', key_id: 'cad-vendor-2026', value_hex: 'b'.repeat(128) },
  }, true],
  ['trust', 'invalid: digest not 64 lowercase hex', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: { digest_sha256: 'ABC' },
  }, false],
  ['trust', 'invalid: unknown signature scheme', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: { digest_sha256: 'a'.repeat(64) },
    signature: { scheme: 'rsa', key_id: 'k', value_hex: 'b'.repeat(128) },
  }, false],
  ['trust', 'invalid: unknown top-level field', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: { digest_sha256: 'a'.repeat(64) },
    extra: true,
  }, false],
  ['trust', 'invalid: missing package digest', {
    schema_version: 1,
    plugin_id: 'cad.validate',
    version: '0.1.0',
    package: {},
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

  // ---- Sidecar envelope v2（M3 L2：host-service 代理） ----
  ['envelope', 'valid: v2 host.service.request + service_key', {
    version: 2, kind: 'host.service.request', request_id: 'req-101',
    runtime_generation: 'node-0007', service_key: 'working-directory/v1',
    session_id: 's-1', depth: 1, trace_id: 't-l2',
    ts: '2026-11-12T10:00:00.000Z', payload: { method: 'set', args: { path: 'C:/work' } },
  }, true],
  ['envelope', 'invalid: v2 service_key without major', {
    version: 2, kind: 'host.service.request', request_id: 'req-102',
    runtime_generation: 'node-0007', service_key: 'working-directory',
    depth: 0, trace_id: 't-l2', ts: '2026-11-12T10:00:00Z', payload: {},
  }, false],
  ['envelope', 'valid: v2 host.service.result with service error', {
    version: 2, kind: 'host.service.result', request_id: 'req-103',
    runtime_generation: 'node-0007', service_key: 'storage-kv/v1',
    depth: 0, trace_id: 't-l2', ts: '2026-11-12T10:00:00Z', payload: {},
    error: { code: 'E_SERVICE_TIMEOUT', message: 'kv timeout', retryable: true },
  }, true],
  // 注：`version:1` 信封里出现 v2-only kind 由运行时协商拒绝（sidecar-rpc.md §3.2），
  // 本校验器不支持 version↔kind 的条件约束，故不在此用 schema 断言。

  // ---- CPP manifest（M3：fuel 上限 / min_host_version / ui.location） ----
  ['manifest', 'valid: fuel at 1e10 + min_host_version + ui location', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1', min_host_version: '1.2.0' },
    runtime: { kind: 'wasm-component', fuel: 10000000000 },
    ui: [{ slot: 'workspace.panel', id: 'x.y.panel', label: 'X', view: 'x.y', location: 'right' }],
  }, true],
  ['manifest', 'invalid: fuel over 1e10', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'wasm-component', fuel: 10000000001 },
  }, false],
  ['manifest', 'invalid: min_host_version not semver', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1', min_host_version: '1.2' },
    runtime: { kind: 'rust-static' },
  }, false],
  ['manifest', 'invalid: ui location not in enum', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'rust-static' },
    ui: [{ slot: 'workspace.panel', id: 'p', label: 'P', view: 'v', location: 'top' }],
  }, false],
  ['manifest', 'invalid: unknown runtime field execution', {
    plugin: { id: 'x.y', version: '1.0.0', api: 'cordis-portable/v1' },
    runtime: { kind: 'rust-static', execution: 'thread' },
  }, false],
]

const validators = {
  element: (data) => validate(schemas.element, data, '$', schemas.element),
  slotRegistration: (data) => validate(schemas.slotRegistration, data, '$', schemas.slotRegistration),
  manifest: (data) => validate(schemas.manifest, data, '$', schemas.manifest),
  trust: (data) => validate(schemas.trust, data, '$', schemas.trust),
  envelope: (data) => validate(schemas.envelope, data, '$', schemas.envelope),
  slots: (data) => checkSlotsCatalog(data),
  registrationContract: (data) => checkRegistration(data),
}

let failures = 0
for (const [schemaName, label, data, expectedValid] of cases) {
  const errors = validators[schemaName](data)
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
