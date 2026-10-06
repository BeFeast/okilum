// Online question actions are never part of the capture outbox.
export function questionStatus(question, operation, online = true) {
  if (operation) return ({
    queued: 'Queued for the source bridge',
    uncertain: 'Delivery unconfirmed — checking the original request; no automatic resend',
    accepted: 'Accepted by T3 — receipt by the executor is not yet confirmed',
    delivered: 'Received by the executor',
    rejected: 'Not sent — the source changed or refused this answer',
  })[operation.state] || 'Unknown delivery status';
  if (question.state === 'answered') return 'Answered at the source';
  if (question.state === 'withdrawn') return 'Withdrawn at the source';
  if (!online) return 'Offline — reconnect and refresh before answering';
  if (!question.source_fresh) return 'Source unavailable or stale — waiting for a fresh check';
  if (!question.can_reply) return 'This question is not currently answerable';
  return 'Waiting for your answer';
}
export function buildReply(question, values, operationId) {
  if (!question.can_reply || !question.source_fresh || question.state !== 'pending' || question.pending_operation_id) {
    throw new Error('Refresh this question before answering.');
  }
  const answers = question.fields.map(field => {
    const value = values[field.id] || {};
    const text = value.text || '', option_ids = value.option_ids || [];
    if (text && option_ids.length) throw new Error('Use either a custom answer or the options, not both.');
    if (!text.trim() && !option_ids.length) throw new Error('Answer every question before sending.');
    if (new TextEncoder().encode(text).length > 16384) throw new Error('Keep each answer under 16 KB.');
    if ((!field.allow_text && text) || (!field.multiple && option_ids.length > 1) ||
      option_ids.some(id => !field.options.some(option => option.id === id)) || new Set(option_ids).size !== option_ids.length) {
      throw new Error('The answer options changed. Refresh the question.');
    }
    return { id: field.id, text, option_ids };
  });
  return { operation_id: operationId, question_id: question.id, expected_revision: question.source_revision, answers };
}
// Save exact consent BEFORE fetch. A reload checks this operation, never makes a
// new one. Explicit retry uses this exact payload only if lookup still says 404.
export function replyJournal(storage, owner) {
  const prefix = `tessera-replies-v1:${owner}:`;
  return {
    get(id) { const raw = storage.getItem(prefix + id); return raw ? JSON.parse(raw) : null; },
    put(body) {
      const previous = storage.getItem(prefix + body.question_id);
      if (previous && previous !== JSON.stringify(body)) throw new Error('An answer is already saved on this device. Check status before sending another.');
      storage.setItem(prefix + body.question_id, JSON.stringify(body));
    },
    refuse(id, operationId) { storage.setItem(prefix + 'refused:' + id,operationId); },
    isRefused(id, operationId) { return storage.getItem(prefix + 'refused:' + id) === operationId; },
    clear(id) { storage.removeItem(prefix + id); storage.removeItem(prefix + 'refused:' + id); },
  };
}

// Old bridge observations combined label and description. Keep meaningful detail,
// but remove a repeated label or a redundant "Choose X" instruction at display time.
export function optionLabel(label) {
  const parts = label.split(' — ');
  if (parts.length !== 2) return label;
  const normalize = value => value.trim().replace(/[.!]+$/, '').toLocaleLowerCase();
  const [name, description] = parts;
  return [normalize(name), `choose ${normalize(name)}`].includes(normalize(description)) ? name : label;
}
