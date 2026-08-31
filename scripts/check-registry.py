import json, sys
data = json.load(open('registry.json'))
assert 'meta' in data, 'missing meta'
assert 'apiVersion' in data['meta'], 'missing meta.apiVersion'
assert isinstance(data.get('revoked', []), list), 'revoked must be list'
count = 0
for slug, entry in data.items():
    if slug in ('meta', 'revoked'):
        continue
    assert 'name' in entry, slug + ' missing name'
    assert 'versions' in entry, slug + ' missing versions'
    assert entry['versions'], slug + ' empty versions'
    count += 1
print('registry.json: %d plugins validated' % count)
