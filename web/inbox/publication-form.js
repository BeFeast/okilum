// Match the server's structural validation before a durable operation is created.
export function publicationProblems(payload, folders = null) {
  const errors = {};
  const component = value => typeof value === 'string' && value.length > 0 && !value.startsWith('.')
    && new TextEncoder().encode(value).length <= 200 && !/[\u0000-\u001f\u007f-\u009f/\\]/u.test(value);
  if (!component(payload.folder) || (folders && !folders.includes(payload.folder))) errors.destination = 'Choose a PARA folder.';
  if (typeof payload.filename !== 'string' || !payload.filename.endsWith('.md')
      || new TextEncoder().encode(payload.filename).length > 1000
      || payload.filename.split('/').length > 10 || !payload.filename.split('/').every(component)) errors.filename = 'Enter a filename or relative subfolder/name; no dot folders or backslashes.';
  if (typeof payload.content !== 'string' || !payload.content.trim()) errors.draft = 'Add the Markdown text to publish.';
  else if (new TextEncoder().encode(payload.content).length > 65536) errors.draft = 'The document exceeds the 64 KB limit. Shorten it before publishing.';
  return errors;
}
export function publicationProblem(payload, folders = null) {
  return Object.values(publicationProblems(payload, folders))[0] || '';
}

// Normalize only a new editable draft, never a saved replay identity.
export function markdownFilename(value) {
  return value && !value.endsWith('.md') ? `${value}.md` : value;
}
export function suggestedFilename(content) {
  const heading = content.match(/^# +(.+?) *#* *$/m)?.[1]
    || content.split('\n').find(line => line.trim()) || '';
  let name = heading.replace(/[\u0000-\u001f\u007f-\u009f/\\:*?"<>|]/gu, ' ').trim().replace(/^\.+/, '').trim();
  while (new TextEncoder().encode(name).length > 190) name = [...name].slice(0, -1).join('');
  return name.replace(/\.md$/, '');
}
export function publicationLabel(row) {
  return row.state === 'published' ? 'Published' : row.conflict === 'file_exists' ? 'Not published: file exists' : row.conflict === 'occupied' ? 'File exists; earlier delivery not confirmed' : 'Not confirmed; retry the saved request';
}
