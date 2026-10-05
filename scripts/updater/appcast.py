#!/usr/bin/env python3
"""Edit the Tessera Sparkle appcast: add a build to a channel, or promote one.

Every item names a channel, `beta` or `stable`; the app always accepts `stable`.
`tessera:source`/`tessera:tree` record the commit each build came from.
"""
import argparse
import email.utils
import sys
import xml.etree.ElementTree as ET

SPARKLE = 'http://www.andymatuschak.org/xml-namespaces/sparkle'
TESSERA = 'https://git.oklabs.uk/BeFeast/tessera/update-metadata'
ET.register_namespace('sparkle', SPARKLE)
ET.register_namespace('tessera', TESSERA)
CHANNELS = ('beta', 'stable')
EMPTY = f'''<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:sparkle="{SPARKLE}" xmlns:tessera="{TESSERA}" version="2.0">
  <channel><title>Tessera updates</title></channel>
</rss>'''


def s(name):
    return f'{{{SPARKLE}}}{name}'


def t(name):
    return f'{{{TESSERA}}}{name}'


def load(path):
    if path is None:
        return ET.ElementTree(ET.fromstring(EMPTY))
    return ET.parse(path)


def items(tree):
    return tree.getroot().find('channel').findall('item')


def find(tree, build):
    for item in items(tree):
        if item.findtext(s('version')) == str(build):
            return item
    return None


def add(tree, a):
    channel = tree.getroot().find('channel')
    if find(tree, a.build) is not None:
        sys.exit(f'Build {a.build} is already in the appcast')
    newest = max((int(i.findtext(s('version'))) for i in items(tree)), default=0)
    if a.build <= newest:
        sys.exit(f'Build {a.build} is not newer than {newest}')
    item = ET.Element('item')
    for tag, text in (
        ('title', f'Tessera {a.short_version}'),
        ('pubDate', email.utils.formatdate(usegmt=True)),
        (s('version'), str(a.build)),
        (s('shortVersionString'), a.short_version),
        (s('channel'), a.channel),
        (s('minimumSystemVersion'), '11.0'),
        (t('source'), a.source),
        (t('tree'), a.tree),
    ):
        ET.SubElement(item, tag).text = text
    ET.SubElement(item, 'enclosure', {
        'url': a.url,
        'length': str(a.length),
        'type': 'application/octet-stream',
        s('edSignature'): a.signature,
        s('installationType'): 'application',
    })
    # Newest first, after the feed title.
    channel.insert(list(channel).index(channel.find('title')) + 1, item)


def promote(tree, a):
    item = find(tree, a.build)
    if item is None:
        sys.exit(f'Build {a.build} is not in the appcast')
    item.find(s('channel')).text = 'stable'


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--appcast', help='current appcast; omit to start an empty one')
    p.add_argument('--output', required=True)
    sub = p.add_subparsers(dest='command', required=True)
    a = sub.add_parser('add')
    a.add_argument('--build', type=int, required=True)
    a.add_argument('--short-version', required=True)
    a.add_argument('--channel', choices=CHANNELS, required=True)
    a.add_argument('--url', required=True)
    a.add_argument('--length', type=int, required=True)
    a.add_argument('--signature', required=True)
    a.add_argument('--source', required=True)
    a.add_argument('--tree', required=True)
    a = sub.add_parser('promote')
    a.add_argument('--build', type=int, required=True)
    args = p.parse_args(argv)
    tree = load(args.appcast)
    {'add': add, 'promote': promote}[args.command](tree, args)
    ET.indent(tree, '  ')
    tree.write(args.output, encoding='utf-8', xml_declaration=True)


if __name__ == '__main__':
    main()
