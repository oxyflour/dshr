import { spawn, spawnSync } from 'node:child_process'
import { closeSync, mkdirSync, openSync, readFileSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import { RUN_TMP_DIR } from './paths.mjs'

// ---------------------------------------------------------------------------
// 进程执行
//
// 注意：这里用临时文件而不是 pipe 捕获 stdout/stderr。原因有两条：
//   1. 某些受限环境（Windows 沙箱 / 容器）禁止子进程通过命名管道回传输出，
//      Node child_process 的 stdio:'pipe' 会直接 EPERM；文件句柄重定向可跨环境工作。
//   2. 文件捕获对长输出（cargo test）更稳，不会因为背压丢字节。
// ---------------------------------------------------------------------------

let runCounter = 0

function killTree(child) {
  if (!child || child.pid === undefined) return
  if (process.platform === 'win32') {
    try {
      spawnSync('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true })
    } catch {
      // 忽略：进程可能已经退出
    }
  }
  try {
    child.kill('SIGKILL')
  } catch {
    // 忽略
  }
}

export function runProcess(command, args, { cwd, timeoutMs, env, combineOutput = false } = {}) {
  return new Promise((resolvePromise) => {
    const startedAt = Date.now()
    runCounter += 1
    mkdirSync(RUN_TMP_DIR, { recursive: true })
    const stem = `run-${process.pid}-${runCounter}-${startedAt}`
    const outPath = join(RUN_TMP_DIR, `${stem}.out`)
    const errPath = join(RUN_TMP_DIR, `${stem}.err`)

    let outFd
    let errFd
    try {
      outFd = openSync(outPath, 'w')
      errFd = combineOutput ? outFd : openSync(errPath, 'w')
    } catch (err) {
      resolvePromise({
        code: null,
        signal: null,
        stdout: '',
        stderr: `无法创建输出捕获文件：${err.message}`,
        timedOut: false,
        spawnError: true,
        durationMs: 0,
      })
      return
    }

    let stdout = ''
    let stderr = ''
    let timedOut = false
    let settled = false
    let timer = null
    let graceTimer = null
    let child = null

    const readFileSafe = (target) => {
      try {
        return readFileSync(target, 'utf8')
      } catch {
        return ''
      }
    }
    const cleanup = () => {
      try {
        rmSync(outPath, { force: true })
        rmSync(errPath, { force: true })
      } catch {
        // 忽略清理失败
      }
    }

    const finish = (code, signal, spawnError = false, spawnMessage = '') => {
      if (settled) return
      settled = true
      if (timer) clearTimeout(timer)
      if (graceTimer) clearTimeout(graceTimer)
      try {
        closeSync(outFd)
      } catch {
        // 忽略
      }
      try {
        closeSync(errFd)
      } catch {
        // 忽略
      }
      stdout = readFileSafe(outPath)
      stderr = combineOutput ? '' : readFileSafe(errPath)
      if (spawnMessage) stderr += `\n${spawnMessage}`
      cleanup()
      resolvePromise({
        code,
        signal,
        stdout,
        stderr,
        timedOut,
        spawnError,
        durationMs: Date.now() - startedAt,
      })
    }

    if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
      timer = setTimeout(() => {
        timedOut = true
        killTree(child)
        // 兜底：即使进程树拒绝退出，也在宽限后结算，避免 harness 永久挂起。
        graceTimer = setTimeout(() => finish(null, 'SIGKILL'), 5000)
      }, timeoutMs)
    }

    try {
      child = spawn(command, args, {
        cwd,
        env: env ?? process.env,
        windowsHide: true,
        // combineOutput：stdout/stderr 写同一个文件，保证 cargo 的 Running 行与
        // test result 行按真实顺序交错（否则 binary→ignored 映射会错位）。
        stdio: ['ignore', outFd, combineOutput ? outFd : errFd],
      })
    } catch (err) {
      finish(null, null, true, `spawn 失败：${err.message}`)
      return
    }

    child.on('error', (err) => {
      finish(null, null, true, `spawn 失败：${err.message}`)
    })
    child.on('close', (code, signal) => finish(code, signal))
  })
}
