# Static code-object inspection only: get_code does not import/execute adapter.
from __future__ import print_function
import zipimport


def find_code(code, name):
    if code.co_name == name:
        return code
    for constant in code.co_consts:
        if type(constant).__name__ == 'code':
            found = find_code(constant, name)
            if found is not None:
                return found
    return None


archive = zipimport.zipimporter(config['vendor_dir'] + '\\library.zip')
root = archive.get_code('adapter')
code = find_code(root, 'sendKey')
if code is None:
    raise ValueError('No static sendKey code object')
print('Static sendKey arguments:', code.co_varnames[:code.co_argcount])
print('Static sendKey names:', code.co_names)
print('Static sendKey constants:', code.co_consts)
labels = {23: 'BINARY_ADD', 83: 'RETURN_VALUE', 100: 'LOAD_CONST', 102: 'BUILD_TUPLE',
          103: 'BUILD_LIST', 106: 'LOAD_ATTR', 116: 'LOAD_GLOBAL', 124: 'LOAD_FAST',
          125: 'STORE_FAST', 131: 'CALL_FUNCTION'}
pc = 0
while pc < len(code.co_code):
    offset = pc
    opcode = ord(code.co_code[pc]); pc += 1
    argument = None
    detail = ''
    if opcode >= 90:
        argument = ord(code.co_code[pc]) | ord(code.co_code[pc + 1]) << 8
        pc += 2
        if opcode in (124, 125):
            detail = code.co_varnames[argument]
        elif opcode == 100:
            detail = repr(code.co_consts[argument])
        elif opcode in (106, 116):
            detail = code.co_names[argument]
    print(offset, labels.get(opcode, str(opcode)), argument, detail)


def show_return_entries(code):
    entries = []
    pc = 0
    while pc < len(code.co_code):
        at = pc
        op = ord(code.co_code[pc]); pc += 1
        arg = None
        value = ''
        if op >= 90:
            arg = ord(code.co_code[pc]) | ord(code.co_code[pc + 1]) << 8
            pc += 2
            if op in (106, 116, 90, 108):
                value = code.co_names[arg]
            elif op == 100:
                value = repr(code.co_consts[arg])
            elif op in (124, 125):
                value = code.co_varnames[arg]
        entries.append((at, labels.get(op, str(op)), arg, value))
    for index, entry in enumerate(entries):
        if 'RETURN' in str(entry[3]).upper() or 'ENTER' in str(entry[3]).upper() or entry[3] in ('65293', '65421'):
            print('Return mapping context:', code.co_name, entries[max(0, index - 3):index + 5])
    for constant in code.co_consts:
        if type(constant).__name__ == 'code':
            show_return_entries(constant)

libkeys_code = archive.get_code('libkeys')
print('Static libkeys names:', libkeys_code.co_names)
show_return_entries(libkeys_code)
adapter_class = find_code(root, 'Adapter')
for member in adapter_class.co_consts:
    if type(member).__name__ == 'code' and member.co_name != '__init__':
        label = member.co_name.lower()
        if any(word in label for word in ('phase', 'pos', 'param', 'video', 'clock', 'sync', 'adjust')):
            print('Static VGA method:', member.co_name, member.co_varnames[:member.co_argcount], member.co_names)

