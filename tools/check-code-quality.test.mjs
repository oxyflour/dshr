import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { test } from 'node:test'

const legacyFile = 'sidecar/legacy.mjs'
const baselineFile = 'tools/code-quality-baseline.json'
const crateManifest = name => `[package]\nname = "${name}"\nversion = "0.0.0"\nedition = "2021"\n`

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'dshr-quality-test-'))
  assert.equal(dirname(cwd), resolve(tmpdir()))
  t.after(() => rmSync(cwd, { recursive: true, force: true }))
  const run = (command, ...args) => spawnSync(command, args, { cwd, encoding: 'utf8' })
  function mustRun(command, ...args) {
    const result = run(command, ...args)
    assert.equal(result.status, 0, result.error?.message ?? result.stderr)
    return result.stdout.trim()
  }
  function write(file, source) {
    mkdirSync(dirname(join(cwd, file)), { recursive: true })
    writeFileSync(join(cwd, file), source)
  }
  function baseline(limits, exceptions = {}) {
    write(baselineFile, JSON.stringify({ limits, exceptions }) + '\n')
  }
  function commit(message) {
    mustRun('git', 'add', '-A')
    mustRun('git', '-c', 'user.name=Quality Test', '-c', 'user.email=quality@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', message)
    return mustRun('git', 'rev-parse', 'HEAD')
  }
  function check(expectedStatus, base = 'HEAD') {
    mustRun('git', 'add', '-A')
    const result = run(process.execPath, 'tools/check-code-quality.mjs', '--base', base)
    const output = result.stdout + result.stderr
    assert.equal(result.status, expectedStatus, result.error?.message ?? output)
    return output
  }
  mustRun('git', 'init', '-q')
  copyFileSync(new URL('../rust-toolchain.toml', import.meta.url), join(cwd, 'rust-toolchain.toml'))
  write('Cargo.toml', '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n')
  for (const name of ['dshr-compat-dsh-protocol', 'dshr-tool-runtime']) {
    write(`crates/${name}/Cargo.toml`, crateManifest(name))
    write(`crates/${name}/src/lib.rs`, 'pub fn example() {}\n')
  }
  mustRun('cargo', 'generate-lockfile', '--offline')
  write(legacyFile, '// legacy\n'.repeat(1002))
  write('README.md', '# Fixture\n')
  baseline({ [legacyFile]: 1002 })
  copyFileSync(new URL('./check-code-quality.mjs', import.meta.url), join(cwd, 'tools/check-code-quality.mjs'))
  commit('fixture')
  return { cwd, write, baseline, check, commit, mustRun }
}

test('quality check reports code violations in a tracked change', (t) => {
  const { check, mustRun, write } = fixture(t)
  assert.match(check(0), /\[code quality\] PASS/)
  write(legacyFile, '// legacy\n'.repeat(1003))
  write('sidecar/new.mjs', '// new\n'.repeat(1001))
  write('sidecar/broken.mjs', 'const =')
  write('sidecar/dsh-official/lib/entry-import.mjs', "import '../sidecar-dsh.mjs'\n")
  write('crates/dshr-tool-runtime/src/lib.rs', 'pub fn   example() {}\n')
  write('apps/desktop-gpui/src/bin/chat/runtime/turn.rs', 'use gpui::Window;\n')
  const crate = 'dshr-compat-dsh-protocol'
  write(`crates/${crate}/Cargo.toml`, crateManifest(crate)
    + '[target.\'cfg(unix)\'.build-dependencies]\ndshr-tool-runtime = { path = "../dshr-tool-runtime" }\n')
  mustRun('cargo', 'generate-lockfile', '--offline')

  const output = check(1)
  assert.match(output, /sidecar\/legacy\.mjs: 1003 lines exceeds its limit of 1002/)
  assert.match(output, /sidecar\/new\.mjs: 1001 lines exceeds its limit of 1000/)
  assert.match(output, /\[Node syntax\].*sidecar\/broken\.mjs/)
  assert.match(output, /\[Rust formatting\]/)
  assert.match(output, /\[dependency direction\] dshr-compat-dsh-protocol -> dshr-tool-runtime \(build, cfg\(unix\)\)/)
  assert.match(output, /turn\.rs: the shared runtime cannot import the desktop view or GPUI/)
  assert.match(output, /entry-import\.mjs: Sidecar libraries cannot import the process entry point/)
})

test('the baseline shrinks and temporary exceptions expire', (t) => {
  const { baseline, check, write } = fixture(t)
  write(legacyFile, '// legacy\n'.repeat(1003))
  baseline({ [legacyFile]: 1003 })
  assert.match(check(1), /the baseline can only shrink/)

  const exception = { maxLines: 1003, owner: 'Quality Test', reason: 'temporary test', expires: '2099-01-01' }
  baseline({ [legacyFile]: 1002 }, { [legacyFile]: exception })
  assert.match(check(0), /\[code quality\] PASS/)
  baseline({ [legacyFile]: 1002 }, { [legacyFile]: { ...exception, expires: '2020-01-01' } })
  assert.match(check(1), /exception expired on 2020-01-01/)

  write(legacyFile, '// legacy\n'.repeat(1001))
  baseline({ [legacyFile]: 1002 })
  assert.match(check(1), /reduce its baseline to 1001/)
  baseline({ [legacyFile]: 1001 })
  assert.match(check(0), /\[code quality\] PASS/)
})

test('the supplied base detects growth from an earlier commit in a push', (t) => {
  const { baseline, check, commit, mustRun, write } = fixture(t)
  const before = mustRun('git', 'rev-parse', 'HEAD')

  write(legacyFile, '// legacy\n'.repeat(1003))
  baseline({ [legacyFile]: 1003 })
  const growth = commit('raise the recorded limit')
  write('README.md', '# Documentation after the limit change\n')
  commit('update the documentation')

  assert.match(check(1, before), /the baseline can only shrink/)
  assert.match(check(0, growth), /\[code quality\] PASS/)
})

test('an in-limit rename, new tests, and a documentation-only change pass', (t) => {
  const { baseline, check, cwd, mustRun, write } = fixture(t)
  write('README.md', '# Documentation only\n')
  assert.match(check(0), /0 changed files need a responsibility review/)

  const newFile = 'sidecar/renamed.mjs'
  renameSync(join(cwd, legacyFile), join(cwd, newFile))
  baseline({})
  assert.match(check(1), /sidecar\/renamed\.mjs: 1002 lines exceeds its limit of 1000/)

  write(newFile, '// legacy\n'.repeat(1000))
  write('sidecar/new.test.mjs', '// test\n'.repeat(1500))
  assert.match(check(0), /\[code quality\] PASS/)
  mustRun('git', 'rm', '-f', '--', newFile)
  assert.match(check(0), /\[code quality\] PASS/)
})
