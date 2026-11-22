import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
export const REGISTRY_PATH = join(REPO_ROOT, 'docs', 'conformance.d', 'registry.json')
export const DEFAULT_JSON_PATH = join(REPO_ROOT, 'docs', 'conformance-matrix.json')
export const DEFAULT_MARKDOWN_PATH = join(REPO_ROOT, 'docs', 'conformance-matrix.md')
export const RUN_TMP_DIR = join(REPO_ROOT, '.tmp', 'conformance')
