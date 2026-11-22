import { setTimeout as delay } from 'node:timers/promises'

export const USER_QUESTIONS_CAP = 'user.questions'
const serviceKey = 'user-questions/v1'

export const USER_QUESTIONS_CATALOG = {
  service_key: serviceKey,
  methods: [
    { name: 'begin', kind: 'write', timeout_ms: 5000, idempotent: false },
    { name: 'poll', kind: 'read', timeout_ms: 5000, idempotent: true },
  ],
}

export function createUserQuestions({ currentCall, callService }) {
  return {
    async ask({ questions, signal }) {
      const call = currentCall()
      if (!call?.sessionId || call.tool !== 'ask_user_question') {
        throw new Error('user questions require an active ask_user_question call')
      }
      const invoke = (method, args) => callService({
        serviceKey,
        method,
        args,
        sessionId: call.sessionId,
        depth: call.depth + 1,
      })
      signal?.throwIfAborted()
      await invoke('begin', { questions: questions.map(question => ({
        id: question.id,
        question: question.question,
        ...(question.header === undefined ? {} : { header: question.header }),
        multiSelect: question.multiSelect ?? false,
        options: (question.options ?? []).map(({ label, description }) => ({
          label,
          ...(description === undefined ? {} : { description }),
        })),
      })) })
      for (;;) {
        const result = await invoke('poll', {})
        if (result.done) return { answers: result.answers }
        await delay(100, undefined, { signal })
      }
    },
  }
}
