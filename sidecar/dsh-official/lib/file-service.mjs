import { lstatSync } from 'node:fs'
import { open, readFile, readdir, realpath, stat as fsStat, lstat as fsLstat } from 'node:fs/promises'
import { isAbsolute, join, relative as relativePath, resolve as resolvePath, sep } from 'node:path'

export const OFFICE_TOOLS = new Set(['office_convert', 'office_recalculate', 'office_render'])

/**
 * 构造一个官方 `FsError`。用真实包是为了让 dsh-tool-fs 的 `instanceof FsError`
 * 判定成立（否则 FS_STALE_VERSION 的"re-read the file"补救提示不会出现）。
 * 包不存在时退回带 `code` 的普通 Error。
 */
async function makeFsError(message, code) {
  try {
    const { FsError } = await import('@deepseek-ai/dsh-fs')
    return new FsError(message, code)
  } catch {
    const error = new Error(message)
    error.code = code
    return error
  }
}

/**
 * Rust 裁决层的错误码 → 官方 dsh-fs 词表。
 *
 * 注意 `FS_SANDBOX_DENIED`：官方 dsh-tool-fs 的 `FsSandboxController.mapError`
 * 只在"挂载了 confining 后端"（`ctx.fs.sandboxMode !== undefined`）时才有 policy；
 * 本侧车是非 confining 后端（真实约束在 Rust），收到 `FS_SANDBOX_DENIED` 会
 * 解引用 undefined 的 policy 而崩溃。因此把 Rust 的沙箱/审批拒绝翻译成
 * `FS_PERMISSION_DENIED`（消息里保留"denied / approval"细节）。
 */
function officialFsErrorCode(code) {
  switch (code) {
    case 'FS_SANDBOX_DENIED':
    case 'E_FS_CAPABILITY':
      return 'FS_PERMISSION_DENIED'
    case 'E_FS_REQUEST':
    case 'E_PROTOCOL':
      return 'FS_IO_ERROR'
    default:
      return code
  }
}

function versionOf(info) {
  // 与 Rust 侧 FsWriteService 的 version_of 同格式：floor(mtime_ms):size。
  return `${Math.floor(info.mtimeMs)}:${info.size}`
}

export function officeOutputPath(root, path) {
  if (typeof path !== 'string' || !isAbsolute(path)) return undefined
  const relative = relativePath(root, path)
  if (!relative || relative === '..' || relative.startsWith(`..${sep}`) || isAbsolute(relative)) {
    return undefined
  }
  let current = root
  for (const segment of relative.split(sep)) {
    current = join(current, segment)
    try {
      if (lstatSync(current).isSymbolicLink()) return undefined
    } catch (error) {
      if (error.code !== 'ENOENT') return undefined
      break
    }
  }
  return resolvePath(path)
}

export function createFsService({ workspaceRoot, currentCall, writeAllowed, requestHost }, options = {}) {
  const root = resolvePath(options.root ?? workspaceRoot)
  const targetMeta = new WeakMap()

  const describeTarget = (absolutePath) => {
    const target = { targetKey: absolutePath, displayPath: absolutePath }
    targetMeta.set(target, withinRoot(absolutePath))
    return target
  }

  const withinRoot = (absolutePath) => {
    const rel = relativePath(root, absolutePath)
    const inside =
      rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel)
    return { inside, relative: rel }
  }

  const metaOf = (target) => {
    const known = targetMeta.get(target)
    if (known) return known
    const absolute = resolvePath(String(target?.targetKey ?? target))
    return withinRoot(absolute)
  }

  const sessionDirectory = async () => {
    const call = currentCall()
    if (!call?.sessionId || !options.workingDirectory) return root
    call.workingDirectory ??= options.workingDirectory.ensure(undefined, call.controller.signal)
    return await call.workingDirectory || root
  }

  const targetOf = async (requestedPath, callOptions) => {
    callOptions.signal?.throwIfAborted()
    const requested = String(requestedPath)
    if (isAbsolute(requested)) return describeTarget(resolvePath(requested))
    const base = typeof callOptions.cwd === 'string' && callOptions.cwd.length > 0
      ? callOptions.cwd
      : await sessionDirectory()
    callOptions.signal?.throwIfAborted()
    return describeTarget(resolvePath(base, requested))
  }

  const checkScopedFile = async (target) => {
    const tool = currentCall()?.tool
    if (tool !== 'present' && !OFFICE_TOOLS.has(tool)) return target.targetKey
    if (!metaOf(target).inside) {
      throw await makeFsError('file path is outside the workspace', 'FS_PERMISSION_DENIED')
    }
    const actual = await realpath(target.targetKey).catch(error => {
      if (error.code !== 'ENOENT') throw error
      return null
    })
    if (!actual) return target.targetKey
    const relative = relativePath(await realpath(root), actual)
    if (!relative || relative === '..' || relative.startsWith(`..${sep}`) || isAbsolute(relative)) {
      throw await makeFsError('file path is outside the workspace', 'FS_PERMISSION_DENIED')
    }
    return actual
  }

  async function readOfficeBytes(target, signal, offset, length, wholeFile) {
    signal?.throwIfAborted()
    const file = await open(await checkScopedFile(target), 'r')
    try {
      const info = await file.stat()
      if (!info.isFile()) throw await makeFsError('Office source must be a regular file', 'FS_NOT_REGULAR_FILE')
      if (wholeFile && info.size > length) {
        throw await makeFsError('Office source exceeds the file size limit', 'FS_TOO_LARGE')
      }
      const data = Buffer.alloc(Math.max(0, Math.min(length, info.size - offset)))
      let position = 0
      while (position < data.length) {
        signal?.throwIfAborted()
        const { bytesRead } = await file.read(data, position, data.length - position, offset + position)
        if (bytesRead === 0) throw await makeFsError('Office source changed during read', 'FS_STALE_VERSION')
        position += bytesRead
      }
      signal?.throwIfAborted()
      return data
    } finally {
      await file.close()
    }
  }

  const callOf = (signal) => {
    const call = currentCall(signal)
    if (!call) throw new Error('filesystem mutation requires an active tool call')
    return call
  }

  const readIfExists = async (target) => {
    try {
      return await readFile(target.targetKey, 'utf8')
    } catch (error) {
      if (error?.code === 'ENOENT') return null
      throw error
    }
  }

  const currentVersion = async (target) => {
    const info = await fsStat(target.targetKey)
    return versionOf(info)
  }

  /**
   * 所有写操作的唯一出口：把官方 fs 语义翻译成 fs.write.request。
   * 路径以"工作区相对路径"发送，Rust 再做一次归一化/边界校验与审批。
   */
  const performWrite = async ({ operation, target, content, expectedVersion, call, intent }) => {
    if (!writeAllowed()) {
      throw await makeFsError(
        'filesystem write is not enabled: host did not negotiate fs.write',
        'FS_PERMISSION_DENIED',
      )
    }
    const meta = metaOf(target)
    if (!meta.inside) {
      throw await makeFsError(
        `path escapes the workspace root "${root}": ${target.displayPath}`,
        'FS_PERMISSION_DENIED',
      )
    }
    const payload = {
      call_id: call.callId,
      tool_request_id: call.toolRequestId,
      tool: call.tool,
      operation,
      path: meta.relative,
      workspace_root: root,
      content: operation === 'write' ? content : undefined,
      expected_version: expectedVersion ?? undefined,
      intent: intent ?? undefined,
      mode: 'workspace-write',
    }
    const result = await requestHost('fs.write.request', payload, {
      call_id: call.callId,
      plugin_id: 'dsh-official',
    }, call.controller.signal)
    if (!result?.ok) {
      const code = result?.error?.code ?? 'FS_IO_ERROR'
      const message = result?.error?.message ?? `write failed (${code})`
      throw await makeFsError(message, officialFsErrorCode(code))
    }
    return result
  }

  return {
    processPath(target) {
      return target.targetKey
    },
    processPathFromHostPath(path) {
      return officeOutputPath(root, path)
    },
    /**
     * 沙箱能力事实：本侧车不挂载 confining 后端（真实约束在 Rust）。
     * 官方 dsh-tool-fs 的 `FsSandboxController` 因此退化为 escalationModes=[]、
     * 无 sandboxPolicy 依赖、无 sandbox_permissions 参数——即"无 escalate 模式"。
     */
    get sandboxMode() {
      return undefined
    },
    async resolve(requestedPath, callOptions = {}) {
      return targetOf(requestedPath, callOptions)
    },
    async stat(target, signal) {
      if (signal?.aborted) throw new Error('stat aborted')
      const path = await checkScopedFile(target)
      try {
        const info = await fsStat(path)
        return {
          version: versionOf(info),
          type: info.isDirectory() ? 'directory' : 'file',
          size: info.size,
        }
      } catch {
        return undefined
      }
    },
    async lstat(requestedPath, callOptions = {}) {
      const target = await targetOf(requestedPath, callOptions)
      await checkScopedFile(target)
      try {
        const info = await fsLstat(target.targetKey)
        return {
          version: versionOf(info),
          type: info.isSymbolicLink()
            ? 'symlink'
            : info.isDirectory()
              ? 'directory'
              : 'file',
          size: info.size,
        }
      } catch {
        return undefined
      }
    },
    async readText(target, signal) {
      return readFile(target.targetKey, { encoding: 'utf8', signal })
    },
    async listDir(target, signal) {
      signal?.throwIfAborted()
      let entries
      try {
        entries = await readdir(target.targetKey, { withFileTypes: true })
      } catch (error) {
        const code = error?.code === 'ENOENT' ? 'FS_NOT_FOUND'
          : error?.code === 'ENOTDIR' ? 'FS_NOT_DIRECTORY'
          : error?.code === 'EACCES' || error?.code === 'EPERM' ? 'FS_PERMISSION_DENIED'
          : 'FS_IO_ERROR'
        throw await makeFsError(String(error?.message ?? error), code)
      }
      signal?.throwIfAborted()
      return entries.map(entry => ({
        name: entry.name,
        target: describeTarget(join(target.targetKey, entry.name)),
        type: entry.isDirectory() ? 'directory'
          : entry.isFile() ? 'file'
          : entry.isSymbolicLink() ? 'symlink' : 'other',
      }))
    },
    async streamText(target, signal) {
      // 工具侧用 `for await` 消费分块；一次返回整块也合法（只是不分块）。
      return [await this.readText(target, signal)]
    },
    async readBytes(target, signal, cap) {
      if (options.images && currentCall()?.tool === 'read_image') {
        return options.images.readBytes(target, signal, cap)
      }
      if (OFFICE_TOOLS.has(currentCall()?.tool)) {
        const limit = Number.isSafeInteger(cap) && cap >= 0 ? Math.min(cap, 100 * 1024 * 1024) : 0
        return readOfficeBytes(target, signal, 0, limit, true)
      }
      if (signal?.aborted) throw new Error('read aborted')
      const buffer = await readFile(target.targetKey)
      return typeof cap === 'number' ? buffer.subarray(0, cap) : buffer
    },
    async readByteRange(target, { offset, length }, signal) {
      if (!OFFICE_TOOLS.has(currentCall()?.tool)
          || !Number.isSafeInteger(offset) || offset < 0
          || !Number.isSafeInteger(length) || length < 0 || length > 64 * 1024) {
        throw await makeFsError('invalid Office byte range', 'FS_PERMISSION_DENIED')
      }
      return readOfficeBytes(target, signal, offset, length, false)
    },
    /**
     * 官方 write 工具调用：无 policy 时是"无条件原子 create-or-overwrite"。
     * `before/after/operation` 是官方 output schema / diff 卡片需要的字段。
     */
    async writeText(target, content, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (intent?.kind === 'createIfAbsent' && before !== null) {
        throw await makeFsError(
          `cannot overwrite existing "${target.displayPath}" without reading it first`,
          'FS_NOT_OBSERVED',
        )
      }
      const expectedVersion =
        typeof intent?.version === 'string' ? intent.version : undefined
      const operation = before === null ? 'create' : 'update'
      const result = await performWrite({
        operation: 'write',
        target,
        content,
        expectedVersion,
        call,
        intent,
      })
      return {
        version: result.version,
        before,
        after: content,
        operation: result.operation ?? operation,
      }
    },
    /** 官方 edit 工具调用：唯一匹配（默认）或 replace_all，写前带版本守卫。 */
    async editText(target, edit, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      if (typeof intent?.version === 'string' && intent.version !== version) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": file changed since it was read`,
          'FS_STALE_VERSION',
        )
      }
      const occurrences = countOccurrences(before, edit.oldString)
      if (occurrences === 0) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": old_string was not found`,
          'FS_EDIT_NOT_FOUND',
        )
      }
      if (occurrences > 1 && !edit.replaceAll) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": old_string appears ${occurrences} times; pass a unique string or replace_all`,
          'FS_AMBIGUOUS_EDIT',
        )
      }
      const after = edit.replaceAll
        ? before.split(edit.oldString).join(edit.newString)
        : replaceOnce(before, edit.oldString, edit.newString)
      const result = await performWrite({
        operation: 'write',
        target,
        content: after,
        expectedVersion: version,
        call,
        intent,
      })
      return { version: result.version, before, after }
    },
    /** 官方 apply_patch update 调用：hunks 由官方 dsh-fs-patch 纯函数应用。 */
    async patchText(target, hunks, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot patch "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      const { applyHunks } = await import('@deepseek-ai/dsh-fs-patch')
      const applied = applyHunks(before.replace(/\r\n/g, '\n'), hunks)
      if (!applied?.ok) {
        throw await makeFsError(
          `cannot apply patch to "${target.displayPath}": hunk did not match the current file`,
          'FS_PATCH_MISMATCH',
        )
      }
      const result = await performWrite({
        operation: 'write',
        target,
        content: applied.after,
        expectedVersion: version,
        call,
        intent,
      })
      return { version: result.version, before, after: applied.after }
    },
    /** 官方 apply_patch delete 调用。 */
    async deleteFile(target, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot delete "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      await performWrite({
        operation: 'delete',
        target,
        expectedVersion: version,
        call,
        intent,
      })
      return { before }
    },
    /**
     * 官方 apply_patch move 调用：写目标 + 删源。两步各自审批、各自快照；
     * 目标写成功而源删除失败时按官方契约返回 sourceFailure（不假装原子）。
     */
    async moveFile(target, destination, moveOptions = {}, signal) {
      const call = callOf(signal)
      const sourceText = await readIfExists(target)
      if (sourceText === null) {
        throw await makeFsError(
          `cannot move "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const sourceVersion = await currentVersion(target)
      if (
        typeof moveOptions.expectedSource?.version === 'string' &&
        moveOptions.expectedSource.version !== sourceVersion
      ) {
        throw await makeFsError(
          `cannot move "${target.displayPath}": file changed since it was read`,
          'FS_STALE_VERSION',
        )
      }
      let after = sourceText
      if (Array.isArray(moveOptions.hunks) && moveOptions.hunks.length > 0) {
        const { applyHunks } = await import('@deepseek-ai/dsh-fs-patch')
        const applied = applyHunks(sourceText.replace(/\r\n/g, '\n'), moveOptions.hunks)
        if (!applied?.ok) {
          throw await makeFsError(
            `cannot apply patch to "${target.displayPath}": hunk did not match the current file`,
            'FS_PATCH_MISMATCH',
          )
        }
        after = applied.after
      }
      const destinationBefore = await readIfExists(destination)
      if (
        moveOptions.expectedDestination?.kind === 'createIfAbsent' &&
        destinationBefore !== null
      ) {
        throw await makeFsError(
          `cannot overwrite existing "${destination.displayPath}" without reading it first`,
          'FS_NOT_OBSERVED',
        )
      }
      const writeResult = await performWrite({
        operation: 'write',
        target: destination,
        content: after,
        expectedVersion:
          typeof moveOptions.expectedDestination?.version === 'string'
            ? moveOptions.expectedDestination.version
            : undefined,
        call,
        intent: moveOptions.expectedDestination,
      })
      const destinationResult = {
        version: writeResult.version,
        before: destinationBefore,
        after,
      }
      try {
        await performWrite({
          operation: 'delete',
          target,
          expectedVersion: sourceVersion,
          call,
          intent: moveOptions.expectedSource,
        })
      } catch (error) {
        return {
          destination: destinationResult,
          sourceDeleted: false,
          sourceBefore: sourceText,
          sourceFailure: {
            code: error?.code ?? 'FS_IO_ERROR',
            message: String(error?.message ?? error),
          },
        }
      }
      return {
        destination: destinationResult,
        sourceDeleted: true,
        sourceBefore: sourceText,
      }
    },
  }
}

function countOccurrences(text, search) {
  if (typeof search !== 'string' || search.length === 0) return 0
  return text.split(search).length - 1
}

function replaceOnce(text, search, replacement) {
  const index = text.indexOf(search)
  if (index < 0) return text
  return text.slice(0, index) + replacement + text.slice(index + search.length)
}
