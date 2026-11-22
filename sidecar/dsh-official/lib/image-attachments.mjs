import { createHash } from 'node:crypto'
import { open, realpath } from 'node:fs/promises'
import { isAbsolute, relative, sep } from 'node:path'
import { SERVICE_KEYS } from './host-services.mjs'

export const IMAGE_ATTACHMENT_CAP = 'attachments.image'

const limits = Object.freeze({
  mediaTypes: ['image/png', 'image/jpeg', 'image/gif', 'image/webp'],
  maxImageBytes: 8 * 1024 * 1024,
  maxMessageImageBytes: 8 * 1024 * 1024,
  maxImagesPerMessage: 20,
  maxImageDimension: 8192,
  maxImagePixels: 16 * 1024 * 1024,
})

export function createImageAttachments({ workspace, currentCall, callService }) {
  const invoke = (method, args) => {
    const call = currentCall()
    if (!call?.sessionId || !['read_image', 'office_render'].includes(call.tool)) {
      throw new Error('image attachments require an active image tool call')
    }
    return callService({
      serviceKey: SERVICE_KEYS.imageAttachments,
      method,
      args,
      sessionId: call.sessionId,
      depth: call.depth + 1,
    })
  }

  async function saveImages(images) {
    const call = currentCall()
    if (call?.tool !== 'office_render' || images.length > limits.maxImagesPerMessage) {
      throw new Error('rendered images require an active Office render call')
    }
    let bytesUsed = 0
    const saved = []
    const pageSize = 48 * 1024
    for (const image of images) {
      const bytes = Buffer.from(image.data)
      bytesUsed += bytes.length
      if (!bytes.length || bytesUsed > limits.maxMessageImageBytes || image.mediaType !== 'image/png') {
        throw new Error('rendered image batch exceeds its PNG attachment limits')
      }
      for (let offset = 0; offset < bytes.length; offset += pageSize) {
        call.controller.signal.throwIfAborted()
        const done = offset + pageSize >= bytes.length
        const result = await invoke('rendered.page', {
          offset, done,
          b64: bytes.subarray(offset, offset + pageSize).toString('base64'),
          ...(done ? { name: image.name } : {}),
        })
        if (done) {
          if (!result?.reference) throw new Error('host did not save the rendered image')
          saved.push(result.reference)
        }
      }
    }
    return saved
  }

  return {
    async readBytes(target, signal, cap) {
      signal?.throwIfAborted()
      const root = await realpath(workspace)
      const path = await realpath(target.targetKey)
      const inWorkspace = relative(root, path)
      if (inWorkspace === '' || inWorkspace === '..' ||
          inWorkspace.startsWith(`..${sep}`) || isAbsolute(inWorkspace)) {
        throw new Error('image path escapes the workspace root')
      }
      const file = await open(path, 'r')
      try {
        const info = await file.stat()
        if (!info.isFile()) throw new Error('image path must be a regular file')
        if (info.size > Math.min(cap, limits.maxImageBytes)) {
          throw new Error('image exceeds the 8 MiB attachment limit')
        }
        const data = Buffer.alloc(info.size)
        let offset = 0
        while (offset < data.length) {
          signal?.throwIfAborted()
          const { bytesRead } = await file.read(data, offset, data.length - offset, offset)
          if (bytesRead === 0) throw new Error('image changed while it was being read')
          offset += bytesRead
        }
        signal?.throwIfAborted()
        currentCall().imageRead = { path: inWorkspace, data }
        return data
      } finally {
        await file.close()
      }
    },
    attachmentService: {
      imageLimits: limits,
      saveImages,
      isAttachmentError(error) {
        return Boolean(error?.detail?.attachment_code)
      },
      async saveImage({ data, mediaType }) {
        const read = currentCall()?.imageRead
        if (!read || read.data !== data) throw new Error('image does not match the active file read')
        try {
          return await invoke('save', {
            path: read.path,
            sha256: createHash('sha256').update(data).digest('hex'),
            media_type: mediaType,
          })
        } catch (error) {
          if (!error.detail?.attachment_code) throw error
          const { AttachmentError } = await import('@deepseek-ai/dsh-attachment')
          throw new AttachmentError(error.message, error.detail.attachment_code)
        }
      },
    },
    modelService: {
      async resolveModelInfo() {
        const { image } = await invoke('route', {})
        return { inputModalities: image ? ['text', 'image'] : ['text'] }
      },
    },
  }
}
