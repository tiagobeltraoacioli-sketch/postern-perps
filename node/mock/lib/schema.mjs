// A small JSON Schema validator (the subset of draft 2020-12 that node/api/schema.json uses), no dependencies,
// so the same shared tests run on the Mac and on the build host against the mock or the real node.
// UNAUDITED prototype.
//
// Supported keywords: $ref (local "#/$defs/..." only), type (string or array), enum, const, properties, required,
// additionalProperties (boolean or schema), items, prefixItems, minItems, maxItems, pattern, minLength, maxLength,
// minimum, maximum, oneOf, anyOf, allOf. An unknown keyword is an ERROR (a schema the validator cannot fully
// apply must not pass silently), except the annotation keywords listed in ANNOTATIONS.

const KNOWN = new Set([
  '$ref', 'type', 'enum', 'const', 'properties', 'required', 'additionalProperties', 'items', 'prefixItems',
  'minItems', 'maxItems', 'pattern', 'minLength', 'maxLength', 'minimum', 'maximum', 'oneOf', 'anyOf', 'allOf',
]);
const ANNOTATIONS = new Set(['$schema', '$id', '$defs', '$comment', 'title', 'description', 'examples', 'default', 'x-unit', 'x-scale']);

function typeOf(v) {
  if (v === null) return 'null';
  if (Array.isArray(v)) return 'array';
  if (typeof v === 'number') return Number.isInteger(v) ? 'integer' : 'number';
  return typeof v;
}
function typeMatches(v, t) {
  const a = typeOf(v);
  return a === t || (t === 'number' && a === 'integer');
}
function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

export class Validator {
  constructor(root) {
    this.root = root;
    this.patterns = new Map();
    this.checkSchema(root, '#');
  }
  /** Refuse a schema that uses a keyword this validator does not implement. */
  checkSchema(s, at) {
    if (typeof s === 'boolean') return;
    if (typeof s !== 'object' || s === null) throw new Error(`schema at ${at} is not an object`);
    for (const k of Object.keys(s)) {
      if (!KNOWN.has(k) && !ANNOTATIONS.has(k)) throw new Error(`unsupported schema keyword ${k} at ${at}`);
    }
    for (const [k, v] of Object.entries(s.$defs ?? {})) this.checkSchema(v, `${at}/$defs/${k}`);
    for (const [k, v] of Object.entries(s.properties ?? {})) this.checkSchema(v, `${at}/properties/${k}`);
    if (typeof s.additionalProperties === 'object') this.checkSchema(s.additionalProperties, `${at}/additionalProperties`);
    if (s.items !== undefined) this.checkSchema(s.items, `${at}/items`);
    for (const [i, v] of (s.prefixItems ?? []).entries()) this.checkSchema(v, `${at}/prefixItems/${i}`);
    for (const key of ['oneOf', 'anyOf', 'allOf']) for (const [i, v] of (s[key] ?? []).entries()) this.checkSchema(v, `${at}/${key}/${i}`);
    if (s.$ref !== undefined) this.resolve(s.$ref);
  }
  resolve(ref) {
    const m = /^#\/\$defs\/([A-Za-z0-9_]+)$/.exec(ref);
    if (!m || !this.root.$defs?.[m[1]]) throw new Error(`unresolvable $ref ${ref}`);
    return this.root.$defs[m[1]];
  }
  /** Validate `value` against the $def `name`. Returns a list of errors (empty when valid). */
  validateDef(name, value) {
    return this.validate({ $ref: `#/$defs/${name}` }, value);
  }
  validate(schema, value, path = '$', errors = []) {
    if (schema === true) return errors;
    if (schema === false) {
      errors.push(`${path}: not allowed`);
      return errors;
    }
    if (schema.$ref) this.validate(this.resolve(schema.$ref), value, path, errors);
    if (schema.type !== undefined) {
      const types = Array.isArray(schema.type) ? schema.type : [schema.type];
      if (!types.some((t) => typeMatches(value, t))) {
        errors.push(`${path}: expected ${types.join('|')}, got ${typeOf(value)}`);
        return errors;
      }
    }
    if (schema.const !== undefined && !deepEqual(schema.const, value)) errors.push(`${path}: expected const ${JSON.stringify(schema.const)}`);
    if (schema.enum && !schema.enum.some((e) => deepEqual(e, value))) errors.push(`${path}: ${JSON.stringify(value)} not in enum`);
    if (typeof value === 'string') {
      if (schema.pattern !== undefined) {
        let re = this.patterns.get(schema.pattern);
        if (!re) this.patterns.set(schema.pattern, (re = new RegExp(schema.pattern, 'u')));
        if (!re.test(value)) errors.push(`${path}: ${JSON.stringify(value).slice(0, 60)} does not match ${schema.pattern}`);
      }
      if (schema.minLength !== undefined && value.length < schema.minLength) errors.push(`${path}: shorter than ${schema.minLength}`);
      if (schema.maxLength !== undefined && value.length > schema.maxLength) errors.push(`${path}: longer than ${schema.maxLength}`);
    }
    if (typeof value === 'number') {
      if (schema.minimum !== undefined && value < schema.minimum) errors.push(`${path}: below ${schema.minimum}`);
      if (schema.maximum !== undefined && value > schema.maximum) errors.push(`${path}: above ${schema.maximum}`);
    }
    if (Array.isArray(value)) {
      if (schema.minItems !== undefined && value.length < schema.minItems) errors.push(`${path}: fewer than ${schema.minItems} items`);
      if (schema.maxItems !== undefined && value.length > schema.maxItems) errors.push(`${path}: more than ${schema.maxItems} items`);
      const pre = schema.prefixItems ?? [];
      pre.forEach((s, i) => i < value.length && this.validate(s, value[i], `${path}[${i}]`, errors));
      if (schema.items !== undefined) for (let i = pre.length; i < value.length; i++) this.validate(schema.items, value[i], `${path}[${i}]`, errors);
    }
    if (typeOf(value) === 'object') {
      for (const k of schema.required ?? []) if (!(k in value)) errors.push(`${path}: missing required ${k}`);
      const props = schema.properties ?? {};
      for (const [k, v] of Object.entries(value)) {
        if (k in props) this.validate(props[k], v, `${path}.${k}`, errors);
        else if (schema.additionalProperties === false) errors.push(`${path}: unexpected property ${k}`);
        else if (typeof schema.additionalProperties === 'object') this.validate(schema.additionalProperties, v, `${path}.${k}`, errors);
      }
    }
    for (const s of schema.allOf ?? []) this.validate(s, value, path, errors);
    if (schema.anyOf && !schema.anyOf.some((s) => this.validate(s, value, path, []).length === 0)) errors.push(`${path}: matches none of anyOf`);
    if (schema.oneOf) {
      const n = schema.oneOf.filter((s) => this.validate(s, value, path, []).length === 0).length;
      if (n !== 1) errors.push(`${path}: matches ${n} of oneOf (exactly 1 required)`);
    }
    return errors;
  }
}
