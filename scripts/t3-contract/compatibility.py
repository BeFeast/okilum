"""Conservative semantic compatibility of Effect JSON-schema documents.

Removed declared fields (including optional fields), RPCs and variants, new
required fields and narrowing fail. Additions and widening are informational.
Unknown schema constructs/ref shapes fail closed rather than guessing.

CONSUMER_EXPECTATIONS is a CURRENT native-client requirement, inspected against
nightly3991, not a fabricated extension of historical snapshot3814. Decoded
roots describe consumer shapes only; actual encoding is exercised by Rust.
"""
import gzip
import hashlib
import json
from pathlib import Path

STREAM = 'OrchestrationV2ThreadStreamItem.decoded'
BOUNDED = 'OrchestrationV2ThreadBoundedSnapshot.decoded'
CONSUMER_EXPECTATIONS = (
    {'root': STREAM, 'variants': {
        'synchronized': {'kind': 'string'},
        'snapshot': {'kind': 'string', 'snapshotSequence': 'integer',
                     'projection': 'object', 'projection.thread': 'object',
                     'projection.thread.id': 'string', 'historyCursor': ('string', 'null'),
                     'hasMoreHistory': 'boolean', 'latestLocalTurnOrdinal': ('integer', 'null')},
        'event': {'kind': 'string', 'sequence': 'integer', 'event': 'object'},
    }},
    {'root': BOUNDED, 'fields': {
        'snapshotSequence': 'integer', 'projection': 'object',
        'projection.thread': 'object', 'projection.thread.id': 'string',
        'historyCursor': ('string', 'null'), 'hasMoreHistory': 'boolean',
        'latestLocalTurnOrdinal': ('integer', 'null'),
    }},
)
ANNOTATIONS = {'title', 'description', 'default', 'examples', '$comment',
               'deprecated', 'readOnly', 'writeOnly', '$id', '$schema'}
KEYWORDS = ANNOTATIONS | {'$ref', '$defs', 'definitions', 'type', 'enum', 'const',
    'anyOf', 'oneOf', 'properties', 'required', 'additionalProperties', 'items',
    'prefixItems', 'minLength', 'maxLength', 'minItems', 'maxItems', 'minProperties',
    'maxProperties', 'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum',
    'multipleOf', 'pattern', 'format', 'uniqueItems'}
TYPES = {'object', 'array', 'string', 'boolean', 'number', 'integer', 'null'}
LOWER = {'minimum', 'exclusiveMinimum', 'minLength', 'minItems', 'minProperties'}
UPPER = {'maximum', 'exclusiveMaximum', 'maxLength', 'maxItems', 'maxProperties'}


class SchemaError(ValueError):
    pass


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True)


class Document:
    def __init__(self, value):
        if not isinstance(value, dict) or 'schema' not in value:
            raise SchemaError('expected Effect schema document with schema root')
        if set(value) - {'schema', 'definitions', '$defs', 'dialect'}:
            raise SchemaError('unknown schema document keys')
        if value.get('dialect', 'draft-2020-12') != 'draft-2020-12':
            raise SchemaError('unsupported JSON-schema dialect')
        self.value = value
        self.root = value['schema']
        self.definitions = value.get('definitions', value.get('$defs', {}))
        if not isinstance(self.definitions, dict):
            raise SchemaError('definitions must be an object')
        self.definition_fingerprint = _json(self.definitions)
        self._checked = set()
        self.validate(self.root)
        for schema in self.definitions.values():
            self.validate(schema)

    def resolve(self, schema):
        visited = set()
        while isinstance(schema, dict) and '$ref' in schema:
            if id(schema) in visited:
                raise SchemaError('reference-only cycle has no concrete schema')
            visited.add(id(schema))
            if set(schema) - ANNOTATIONS - {'$ref'}:
                raise SchemaError('unsupported constraints beside $ref')
            reference = schema['$ref']
            if not isinstance(reference, str) or not reference.startswith('#/'):
                raise SchemaError('only local JSON-pointer references are supported')
            parts = [part.replace('~1', '/').replace('~0', '~') for part in reference[2:].split('/')]
            if parts[0] in {'$defs', 'definitions'}:
                node = self.definitions
                parts = parts[1:]
            else:
                node = self.root
            try:
                for part in parts:
                    node = node[part]
            except (KeyError, TypeError):
                raise SchemaError('unresolved local schema reference') from None
            schema = node
        return schema

    def validate(self, schema):
        if isinstance(schema, bool):
            return
        if not isinstance(schema, dict):
            raise SchemaError('schema must be an object or boolean')
        if id(schema) in self._checked:
            return
        self._checked.add(id(schema))
        unknown = set(schema) - KEYWORDS
        if unknown:
            raise SchemaError('unsupported schema keywords: ' + ', '.join(sorted(unknown)))
        if '$ref' in schema:
            self.validate(self.resolve(schema))
            return
        kind = schema.get('type')
        if kind is not None and not (
                isinstance(kind, str) and kind in TYPES or
                isinstance(kind, list) and kind and all(isinstance(x, str) and x in TYPES for x in kind)):
            raise SchemaError('invalid schema type')
        required = schema.get('required', [])
        if not isinstance(required, list) or not all(isinstance(x, str) for x in required):
            raise SchemaError('required must be an array of property names')
        if 'enum' in schema and (not isinstance(schema['enum'], list) or not schema['enum']):
            raise SchemaError('enum must be a nonempty array')
        for key in ('anyOf', 'oneOf', 'prefixItems'):
            if key in schema:
                if not isinstance(schema[key], list) or (key != 'prefixItems' and not schema[key]):
                    raise SchemaError(key + ' must be an array of schemas')
                for child in schema[key]:
                    self.validate(child)
        if 'anyOf' in schema and 'oneOf' in schema:
            raise SchemaError('combined anyOf and oneOf is not supported')
        if 'oneOf' in schema:
            # Adding overlapping oneOf branches narrows rather than widens.
            # Only disjoint types/literals/required discriminators are safe to
            # handle with the additive-variant policy used by this checker.
            branches = [self.resolve(x) for x in schema['oneOf']]
            for index, left in enumerate(branches):
                for right in branches[index + 1:]:
                    lt, rt = set(_types(left)), set(_types(right))
                    if 'number' in lt:
                        lt.add('integer')
                    if 'number' in rt:
                        rt.add('integer')
                    ll, rl = self.literals(left), self.literals(right)
                    la, ra = self.tags(left), self.tags(right)
                    disjoint = (not lt & rt or
                                ll is not None and rl is not None and not ll & rl or
                                any(not la[key] & ra[key] for key in set(la) & set(ra)))
                    if not disjoint:
                        raise SchemaError('overlapping/ambiguous oneOf variants are unsupported')
        for key in ('properties', '$defs', 'definitions'):
            if key in schema:
                if not isinstance(schema[key], dict):
                    raise SchemaError(key + ' must be an object')
                for child in schema[key].values():
                    self.validate(child)
        for key in ('items', 'additionalProperties'):
            if key in schema:
                self.validate(schema[key])
        for key in LOWER | UPPER | {'multipleOf'}:
            if key in schema and (isinstance(schema[key], bool) or not isinstance(schema[key], (int, float))):
                raise SchemaError(key + ' must be numeric')
        for key in ('pattern', 'format'):
            if key in schema and not isinstance(schema[key], str):
                raise SchemaError(key + ' must be a string')
        if 'uniqueItems' in schema and not isinstance(schema['uniqueItems'], bool):
            raise SchemaError('uniqueItems must be boolean')

    def branches(self, schema):
        schema = self.resolve(schema)
        if isinstance(schema, dict) and ('anyOf' in schema or 'oneOf' in schema):
            # A union with sibling constraints cannot be treated as a plain union.
            if set(schema) - ANNOTATIONS - {'anyOf', 'oneOf'}:
                raise SchemaError('unsupported constraints beside union')
            result = []
            for branch in schema.get('anyOf', schema.get('oneOf')):
                result.extend(self.branches(branch))
            return result
        return [schema]

    def literals(self, schema):
        schema = self.resolve(schema)
        if isinstance(schema, dict):
            if 'const' in schema:
                return {_json(schema['const'])}
            if 'enum' in schema:
                return {_json(x) for x in schema['enum']}
        return None

    def tags(self, schema, depth=0):
        """Stable discriminator paths, including nested event.type, never array indices."""
        schema = self.resolve(schema)
        if not isinstance(schema, dict) or depth > 3:
            return {}
        result = {}
        required = set(schema.get('required', []))
        for name, child in schema.get('properties', {}).items():
            if name not in required:
                continue
            values = self.literals(child)
            if values:
                result[name] = values
            for path, values in self.tags(child, depth + 1).items():
                result[name + '.' + path] = values
        return result

    def fingerprint(self, schema, seen=None):
        schema = self.resolve(schema)
        if not isinstance(schema, dict):
            return _json(schema)
        seen = set() if seen is None else seen
        if id(schema) in seen:
            return '<recursive>'
        seen = seen | {id(schema)}
        result = {}
        for key, value in schema.items():
            if key in ANNOTATIONS or key in {'$defs', 'definitions'}:
                continue
            if key == 'properties':
                result[key] = {k: self.fingerprint(v, seen) for k, v in value.items()}
            elif key in {'anyOf', 'oneOf', 'prefixItems'}:
                items = [self.fingerprint(v, seen) for v in value]
                result[key] = items if key == 'prefixItems' else sorted(items)
            elif key in {'items', 'additionalProperties'}:
                result[key] = self.fingerprint(value, seen)
            elif key in {'required', 'enum'}:
                result[key] = sorted(_json(x) for x in value)
            else:
                result[key] = value
        # Hash child digests rather than serializing recursively escaped JSON.
        # Nested schemas otherwise grow exponentially during union matching.
        return hashlib.sha256(_json(result).encode()).hexdigest()


def _types(schema):
    if schema is True or schema == {}:
        return TYPES
    if schema is False:
        return set()
    value = schema.get('type')
    if value is None:
        return TYPES
    return {value} if isinstance(value, str) else set(value)


def _accepts(types, kind):
    return kind in types or kind == 'integer' and 'number' in types


class Comparison:
    def __init__(self):
        self.result = {'status': 'PASS', 'breaking': [], 'informational': [], 'errors': []}
        self.seen = set()

    def note(self, bucket, path, message):
        self.result[bucket].append({'path': path, 'message': message})

    def node(self, old, new, od, nd, path):
        old, new = od.resolve(old), nd.resolve(new)
        pair = (id(old), id(new), id(od), id(nd))
        if pair in self.seen:
            return
        self.seen.add(pair)
        # Equal-looking $refs may resolve to changed definitions. Walk objects
        # even when their inline JSON is identical; pair memoization ends cycles.
        if old == new and od.definition_fingerprint == nd.definition_fingerprint:
            return
        # Reject lost concrete contracts before inspecting a possibly huge
        # union. Unknown/opaque output must never hide removed declared fields.
        if new == {} and old not in ({}, True):
            raise SchemaError('concrete schema became opaque')
        if old is False or new is True or new == {}:
            self.note('informational', path, 'schema widened')
            return
        if new is False or old is True or old == {}:
            self.note('breaking', path, 'schema narrowed from an unconstrained contract')
            return
        if isinstance(old, dict) and ('anyOf' in old or 'oneOf' in old) or isinstance(new, dict) and ('anyOf' in new or 'oneOf' in new):
            if ('oneOf' in old) != ('oneOf' in new):
                raise SchemaError('union operator changed; compatibility is not established')
            self.union(od.branches(old), nd.branches(new), od, nd, path)
            return
        for kind in sorted(_types(old)):
            if not _accepts(_types(new), kind):
                self.note('breaking', path, 'type removed or narrowed: ' + kind)
        for kind in sorted(_types(new)):
            if not _accepts(_types(old), kind):
                self.note('informational', path, 'type added or widened: ' + kind)
        old_enum, new_enum = od.literals(old), nd.literals(new)
        if new_enum is not None and (old_enum is None or not old_enum <= new_enum):
            self.note('breaking', path, 'enum/constant values removed or narrowed')
        if old_enum is not None and (new_enum is None or not new_enum <= old_enum):
            self.note('informational', path, 'enum/constant values added or widened')
        op, np = old.get('properties', {}), new.get('properties', {})
        for name in sorted(set(op) - set(np)):
            self.note('breaking', path + '.' + name, 'declared field removed (optional fields included)')
        for name in sorted(set(np) - set(op)):
            self.note('informational', path + '.' + name, 'field added')
        for name in sorted(set(op) & set(np)):
            self.node(op[name], np[name], od, nd, path + '.' + name)
        for name in sorted(set(new.get('required', [])) - set(old.get('required', []))):
            self.note('breaking', path + '.' + name, 'field became required')
        for name in sorted(set(old.get('required', [])) - set(new.get('required', []))):
            self.note('informational', path + '.' + name, 'field became optional')
        if 'additionalProperties' in old or 'additionalProperties' in new:
            self.node(old.get('additionalProperties', True), new.get('additionalProperties', True), od, nd, path + '.*')
        if 'items' in old or 'items' in new:
            self.node(old.get('items', True), new.get('items', True), od, nd, path + '[]')
        ot, nt = old.get('prefixItems', []), new.get('prefixItems', [])
        if len(ot) != len(nt):
            raise SchemaError('changed tuple arity needs explicit compatibility handling')
        for index, (a, b) in enumerate(zip(ot, nt)):
            self.node(a, b, od, nd, path + f'[{index}]')
        for key in LOWER | UPPER:
            a, b = old.get(key), new.get(key)
            if a == b:
                continue
            narrowing = b is not None and (a is None or (b > a if key in LOWER else b < a))
            self.note('breaking' if narrowing else 'informational', path, key + (' narrowed' if narrowing else ' widened'))
        for key in ('pattern', 'format', 'multipleOf', 'uniqueItems'):
            a, b = old.get(key), new.get(key)
            if a == b:
                continue
            if key == 'uniqueItems':
                narrowing = b is True
            else:
                narrowing = b is not None
            self.note('breaking' if narrowing else 'informational', path, key + (' changed/narrowed' if narrowing else ' removed/widened'))

    def union(self, old, new, od, nd, path, used=frozenset()):
        if len(old) == len(new) == 1:
            self.node(old[0], new[0], od, nd, path)
            return
        tags_old = [od.tags(x) for x in old]
        tags_new = [nd.tags(x) for x in new]
        shared = set.intersection(*(set(x) for x in tags_old + tags_new)) if old and new else set()
        choices = []
        for key in shared - used:
            values = set().union(*(x[key] for x in tags_old + tags_new))
            if len(values) > 1:
                choices.append(key)
        if choices:
            key = sorted(choices, key=lambda x: (x not in {'type', 'kind', '_tag', 'tag'}, x.count('.'), x))[0]
            def groups(branches, tags):
                result = {}
                for branch, tag in zip(branches, tags):
                    for value in tag[key]:
                        result.setdefault(value, []).append(branch)
                return result
            a, b = groups(old, tags_old), groups(new, tags_new)
            for value in sorted(set(a) - set(b)):
                self.note('breaking', path + '[' + key + '=' + value + ']', 'variant removed')
            for value in sorted(set(b) - set(a)):
                self.note('informational', path + '[' + key + '=' + value + ']', 'variant added')
            for value in sorted(set(a) & set(b)):
                self.union(a[value], b[value], od, nd, path + '[' + key + '=' + value + ']', used | {key})
            return
        # Match exact semantic shapes first. Order and definition names are not identities.
        a = {od.fingerprint(x): x for x in old}
        b = {nd.fingerprint(x): x for x in new}
        common = set(a) & set(b)
        old_left = [x for k, x in a.items() if k not in common]
        new_left = [x for k, x in b.items() if k not in common]
        if common and old_left and new_left:
            # An unchanged branch (for example null) can hide discriminators
            # shared by the remaining objects. Reconsider only the unmatched
            # branches; each retry removes at least one exact semantic shape.
            self.union(old_left, new_left, od, nd, path, used)
            return
        if len(old_left) == len(new_left) == 1:
            self.node(old_left[0], new_left[0], od, nd, path + '[changed variant]')
        elif not old_left:
            for _ in new_left:
                self.note('informational', path, 'union variant added')
        elif not new_left:
            for _ in old_left:
                self.note('breaking', path, 'union variant removed')
        else:
            # Primitive unions (e.g. nullable strings) match by type, not position.
            def primitive_groups(branches):
                result = {}
                for branch in branches:
                    kind = tuple(sorted(_types(branch)))
                    if kind in result or 'object' in kind or 'array' in kind:
                        return None
                    result[kind] = branch
                return result
            oa, nb = primitive_groups(old_left), primitive_groups(new_left)
            if oa is None or nb is None:
                raise SchemaError('ambiguous untagged union change; cannot establish variant identity')
            for kind in set(oa) & set(nb):
                self.node(oa[kind], nb[kind], od, nd, path + '[' + '|'.join(kind) + ']')
            for kind in set(oa) - set(nb):
                self.note('breaking', path, 'union type variant removed: ' + '|'.join(kind))
            for kind in set(nb) - set(oa):
                self.note('informational', path, 'union type variant added: ' + '|'.join(kind))


def _field(doc, schema, path):
    for name in path.split('.'):
        schema = doc.resolve(schema)
        if not isinstance(schema, dict) or name not in schema.get('properties', {}):
            raise SchemaError('consumer field missing: ' + path)
        schema = schema['properties'][name]
    return schema


def _field_types(doc, schema):
    return set().union(*(_types(x) for x in doc.branches(schema)))


def _consumer(candidate, requirements, result):
    for requirement in requirements:
        root = requirement['root']
        try:
            if root not in candidate:
                raise SchemaError('required decoded consumer root missing')
            doc = Document(candidate[root])
            if doc.resolve(doc.root) in ({}, True, False):
                raise SchemaError('decoded consumer root must be concrete/nonopaque')
            if 'variants' in requirement:
                variants = {}
                for branch in doc.branches(doc.root):
                    tags = doc.tags(branch)
                    for value in tags.get('kind', set()):
                        variants[value] = branch
                scopes = []
                for variant, fields in requirement['variants'].items():
                    if _json(variant) not in variants:
                        raise SchemaError('consumer stream variant missing: ' + variant)
                    scopes.append((variants[_json(variant)], fields))
            else:
                scopes = [(doc.root, requirement['fields'])]
            for schema, fields in scopes:
                for path, expected in fields.items():
                    field = _field(doc, schema, path)
                    allowed = {expected} if isinstance(expected, str) else set(expected)
                    if _field_types(doc, field) != allowed:
                        raise SchemaError('consumer field type changed or opaque: ' + path)
        except SchemaError as error:
            result.note('errors', root, str(error))


def compare(candidate, baseline, *, expectations=CONSUMER_EXPECTATIONS):
    """Public API shared by hosted nightly and the offline candidate probe."""
    result = Comparison()
    if not isinstance(candidate, dict) or not isinstance(baseline, dict) or not baseline:
        result.note('errors', '$', 'candidate and nonempty baseline must be schema-root mappings')
    else:
        _consumer(candidate, expectations, result)
        for root in sorted(set(baseline) - set(candidate)):
            result.note('breaking', root, 'RPC/schema root removed')
        for root in sorted(set(candidate) - set(baseline)):
            result.note('informational', root, 'RPC/schema root added')
        for root in sorted(set(candidate) | set(baseline)):
            try:
                old = Document(baseline[root]) if root in baseline else None
                new = Document(candidate[root]) if root in candidate else None
                if old is not None and new is not None:
                    result.node(old.root, new.root, old, new, root)
            except (SchemaError, RecursionError) as error:
                result.note('errors', root, str(error))
    if result.result['breaking'] or result.result['errors']:
        result.result['status'] = 'FAIL'
    return result.result


def load_baseline(path=None):
    """Merge historical RPC docs with separately attributed current decoded docs.

    A custom baseline can replace an explicit decoded root, but cannot silently
    omit protection of the current consumer surfaces.
    """
    directory = Path(__file__).resolve().parent
    historical = json.loads(Path(path or directory / 'snapshot.json').read_text())
    metadata = json.loads((directory / 'consumer-baseline.json').read_text())
    packed = (directory / metadata['snapshot']).read_bytes()
    if hashlib.sha256(packed).hexdigest() != metadata['sha256']:
        raise SchemaError('current consumer baseline compressed digest mismatch')
    raw = gzip.decompress(packed)
    if hashlib.sha256(raw).hexdigest() != metadata['decoded_json_sha256']:
        raise SchemaError('current consumer baseline decoded digest mismatch')
    consumer = json.loads(raw)
    if set(consumer) != set(metadata['roots']) or set(consumer) != {STREAM, BOUNDED}:
        raise SchemaError('current consumer baseline roots mismatch')
    if not isinstance(historical, dict) or not historical:
        raise SchemaError('historical baseline must be a nonempty root mapping')
    result = dict(historical)
    for root, document in consumer.items():
        result.setdefault(root, document)
    return result
