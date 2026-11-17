// Minimal, strict, event-based XML reader for the two OFAC publications (SDN.XML, SDN_ADVANCED.XML).
// UNAUDITED prototype, no dependencies. Strict means: anything outside the subset these files use is an error,
// never guessed: a DOCTYPE (no DTD, no entity expansion), an unknown entity, an unterminated construct, or a
// mismatched end tag throws XmlError. Namespaces are kept as written (prefix:name); OFAC uses a default namespace.

export class XmlError extends Error {
  constructor(msg, pos) { super(`xml: ${msg} at offset ${pos}`); this.name = 'XmlError'; }
}

const ENT = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" };

export function decodeEntities(s, pos = 0) {
  if (s.indexOf('&') < 0) return s;
  const amps = (s.match(/&/g) || []).length;
  const refs = (s.match(/&[^;&\s]{1,10};/g) || []).length;
  if (amps !== refs) throw new XmlError('bare & in text', pos);
  return s.replace(/&([^;&\s]{1,10});/g, (m, name) => {
    if (Object.hasOwn(ENT, name)) return ENT[name];
    let cp = null;
    if (/^#x[0-9a-fA-F]{1,6}$/.test(name)) cp = parseInt(name.slice(2), 16);
    else if (/^#[0-9]{1,7}$/.test(name)) cp = parseInt(name.slice(1), 10);
    if (cp === null || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) throw new XmlError(`unknown entity &${name};`, pos);
    return String.fromCodePoint(cp);
  });
}

function parseAttrs(src, pos) {
  const attrs = {};
  const re = /\s*([A-Za-z_][\w.:-]*)\s*=\s*("([^"]*)"|'([^']*)')/y;
  let i = 0;
  while (i < src.length) {
    if (/^\s*$/.test(src.slice(i))) break;
    re.lastIndex = i;
    const m = re.exec(src);
    if (!m) throw new XmlError(`bad attribute syntax ${JSON.stringify(src.slice(i, i + 40))}`, pos);
    if (Object.hasOwn(attrs, m[1])) throw new XmlError(`duplicate attribute ${m[1]}`, pos);
    attrs[m[1]] = decodeEntities(m[3] ?? m[4], pos);
    i = re.lastIndex;
  }
  return attrs;
}

/**
 * Walk `text` and call handlers: onOpen(name, attrs, path), onText(text, path), onClose(name, path).
 * `path` is the array of open element names (the open element last). Returns the number of elements.
 */
export function walk(text, { onOpen = () => {}, onText = () => {}, onClose = () => {} } = {}) {
  const stack = [];
  let i = 0; let count = 0; let rootSeen = false;
  const n = text.length;
  if (text.charCodeAt(0) === 0xfeff) i = 1;
  while (i < n) {
    const lt = text.indexOf('<', i);
    const end = lt < 0 ? n : lt;
    if (end > i) {
      const raw = text.slice(i, end);
      if (stack.length === 0) {
        if (!/^\s*$/.test(raw)) throw new XmlError('text outside the root element', i);
      } else {
        onText(decodeEntities(raw, i), stack);
      }
    }
    if (lt < 0) break;
    if (text.startsWith('<?', lt)) {
      const e = text.indexOf('?>', lt + 2); if (e < 0) throw new XmlError('unterminated <?', lt); i = e + 2; continue;
    }
    if (text.startsWith('<!--', lt)) {
      const e = text.indexOf('-->', lt + 4); if (e < 0) throw new XmlError('unterminated comment', lt); i = e + 3; continue;
    }
    if (text.startsWith('<![CDATA[', lt)) {
      const e = text.indexOf(']]>', lt + 9); if (e < 0) throw new XmlError('unterminated CDATA', lt);
      if (stack.length === 0) throw new XmlError('CDATA outside the root', lt);
      onText(text.slice(lt + 9, e), stack); i = e + 3; continue;
    }
    if (text.startsWith('<!', lt)) throw new XmlError('DOCTYPE or declaration refused', lt);
    const gt = text.indexOf('>', lt + 1);
    if (gt < 0) throw new XmlError('unterminated tag', lt);
    const body = text.slice(lt + 1, gt);
    if (body.startsWith('/')) {
      const name = body.slice(1).trim();
      const top = stack.pop();
      if (top !== name) throw new XmlError(`end tag </${name}> does not match <${top}>`, lt);
      onClose(name, stack);
    } else {
      const selfClose = body.endsWith('/');
      const inner = selfClose ? body.slice(0, -1) : body;
      const m = /^([A-Za-z_][\w.:-]*)([\s\S]*)$/.exec(inner);
      if (!m) throw new XmlError(`bad tag ${JSON.stringify(body.slice(0, 40))}`, lt);
      if (stack.length === 0) { if (rootSeen) throw new XmlError('second root element', lt); rootSeen = true; }
      const attrs = parseAttrs(m[2], lt);
      stack.push(m[1]); count++;
      onOpen(m[1], attrs, stack);
      if (selfClose) { stack.pop(); onClose(m[1], stack); }
    }
    i = gt + 1;
  }
  if (stack.length) throw new XmlError(`unclosed <${stack[stack.length - 1]}>`, n);
  if (!rootSeen) throw new XmlError('no root element', 0);
  return count;
}
