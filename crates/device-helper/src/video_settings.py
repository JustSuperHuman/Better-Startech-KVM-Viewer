# Static Python-2 code-object inspection. Never execute the inspected modules.
from __future__ import print_function
import zipimport


DEFAULT_PROBES = [
    ('actions', 'Actions.CMD_videoSettings', 0),
    ('wxer', 'VideoSettingsWindow.__init__', 0),
    ('wxer', 'VideoSettingsWindow.positionTweak', 0),
    ('wxer', 'VideoSettingsWindow.positionAuto', 0),
    ('wxer', 'VideoSettingsWindow.positionDefault', 0),
    ('adapter', 'Adapter.sendMiscValue', 0),
]
# Complete CPython 2.7 opcode table; unknown values still print numerically.
NAMES = {
    0: 'STOP_CODE', 1: 'POP_TOP', 2: 'ROT_TWO', 3: 'ROT_THREE', 4: 'DUP_TOP', 5: 'ROT_FOUR',
    9: 'NOP', 10: 'UNARY_POSITIVE', 11: 'UNARY_NEGATIVE', 12: 'UNARY_NOT', 13: 'UNARY_CONVERT',
    15: 'UNARY_INVERT', 19: 'BINARY_POWER', 20: 'BINARY_MULTIPLY', 21: 'BINARY_DIVIDE',
    22: 'BINARY_MODULO', 23: 'BINARY_ADD', 24: 'BINARY_SUBTRACT', 25: 'BINARY_SUBSCR',
    26: 'BINARY_FLOOR_DIVIDE', 27: 'BINARY_TRUE_DIVIDE', 28: 'INPLACE_FLOOR_DIVIDE',
    29: 'INPLACE_TRUE_DIVIDE', 30: 'SLICE+0', 31: 'SLICE+1', 32: 'SLICE+2', 33: 'SLICE+3',
    40: 'STORE_SLICE+0', 41: 'STORE_SLICE+1', 42: 'STORE_SLICE+2', 43: 'STORE_SLICE+3',
    50: 'DELETE_SLICE+0', 51: 'DELETE_SLICE+1', 52: 'DELETE_SLICE+2', 53: 'DELETE_SLICE+3',
    54: 'STORE_MAP', 55: 'INPLACE_ADD', 56: 'INPLACE_SUBTRACT', 57: 'INPLACE_MULTIPLY',
    58: 'INPLACE_DIVIDE', 59: 'INPLACE_MODULO', 60: 'STORE_SUBSCR', 61: 'DELETE_SUBSCR',
    62: 'BINARY_LSHIFT', 63: 'BINARY_RSHIFT', 64: 'BINARY_AND', 65: 'BINARY_XOR', 66: 'BINARY_OR',
    67: 'INPLACE_POWER', 68: 'GET_ITER', 70: 'PRINT_EXPR', 71: 'PRINT_ITEM', 72: 'PRINT_NEWLINE',
    73: 'PRINT_ITEM_TO', 74: 'PRINT_NEWLINE_TO', 75: 'INPLACE_LSHIFT', 76: 'INPLACE_RSHIFT',
    77: 'INPLACE_AND', 78: 'INPLACE_XOR', 79: 'INPLACE_OR', 80: 'BREAK_LOOP', 81: 'WITH_CLEANUP',
    82: 'LOAD_LOCALS', 83: 'RETURN_VALUE', 84: 'IMPORT_STAR', 85: 'EXEC_STMT', 86: 'YIELD_VALUE',
    87: 'POP_BLOCK', 88: 'END_FINALLY', 89: 'BUILD_CLASS', 90: 'STORE_NAME', 91: 'DELETE_NAME',
    92: 'UNPACK_SEQUENCE', 93: 'FOR_ITER', 94: 'LIST_APPEND', 95: 'STORE_ATTR', 96: 'DELETE_ATTR',
    97: 'STORE_GLOBAL', 98: 'DELETE_GLOBAL', 99: 'DUP_TOPX', 100: 'LOAD_CONST', 101: 'LOAD_NAME',
    102: 'BUILD_TUPLE', 103: 'BUILD_LIST', 104: 'BUILD_SET', 105: 'BUILD_MAP', 106: 'LOAD_ATTR',
    107: 'COMPARE_OP', 108: 'IMPORT_NAME', 109: 'IMPORT_FROM', 110: 'JUMP_FORWARD',
    111: 'JUMP_IF_FALSE_OR_POP', 112: 'JUMP_IF_TRUE_OR_POP', 113: 'JUMP_ABSOLUTE',
    114: 'POP_JUMP_IF_FALSE', 115: 'POP_JUMP_IF_TRUE', 116: 'LOAD_GLOBAL', 119: 'CONTINUE_LOOP',
    120: 'SETUP_LOOP', 121: 'SETUP_EXCEPT', 122: 'SETUP_FINALLY', 124: 'LOAD_FAST',
    125: 'STORE_FAST', 126: 'DELETE_FAST', 130: 'RAISE_VARARGS', 131: 'CALL_FUNCTION',
    132: 'MAKE_FUNCTION', 133: 'BUILD_SLICE', 134: 'MAKE_CLOSURE', 135: 'LOAD_CLOSURE',
    136: 'LOAD_DEREF', 137: 'STORE_DEREF', 140: 'CALL_FUNCTION_VAR', 141: 'CALL_FUNCTION_KW',
    142: 'CALL_FUNCTION_VAR_KW', 143: 'SETUP_WITH', 145: 'EXTENDED_ARG', 146: 'SET_ADD',
    147: 'MAP_ADD',
}
COMPARISONS = ('<', '<=', '==', '!=', '>', '>=', 'in', 'not in', 'is', 'is not', 'exception match')
RELATIVE_JUMPS = (93, 110, 120, 121, 122, 143)
ABSOLUTE_JUMPS = (111, 112, 113, 114, 115, 119)
PAGE = 500


def select(code, path):
    if not path:
        return code
    for item in code.co_consts:
        if type(item).__name__ == 'code' and item.co_name == path[0]:
            return select(item, path[1:])
    raise ValueError('Static code object not found: ' + '.'.join(path))


def line_starts(code):
    starts, line, address, last = {}, code.co_firstlineno, 0, None
    table = code.co_lnotab
    for index in xrange(0, len(table), 2):
        if ord(table[index]):
            if line != last:
                starts[address] = line
                last = line
            address += ord(table[index])
        line += ord(table[index + 1])
    if line != last:
        starts[address] = line
    return starts


def show(code, start):
    print('SOURCE:', code.co_filename, 'line', code.co_firstlineno, 'bytes', len(code.co_code))
    print('ARGS:', code.co_varnames[:code.co_argcount], 'DEFAULTS: see enclosing MAKE_FUNCTION')
    print('NAMES:', repr(code.co_names))
    print('CONSTANTS:', repr(code.co_consts))
    starts = line_starts(code)
    at, index, emitted, extended = 0, 0, 0, 0
    while at < len(code.co_code) and emitted < PAGE:
        offset = at
        op = ord(code.co_code[at]); at += 1
        arg, detail = None, ''
        if op >= 90:
            arg = (ord(code.co_code[at]) | ord(code.co_code[at + 1]) << 8) + extended
            at += 2
            extended = (arg << 16) if op == 145 else 0
            if op == 100:
                detail = repr(code.co_consts[arg])
            elif op in (90, 91, 95, 96, 97, 98, 101, 106, 108, 109, 116):
                detail = code.co_names[arg]
            elif op in (124, 125, 126):
                detail = code.co_varnames[arg]
            elif op in (135, 136, 137):
                detail = (code.co_cellvars + code.co_freevars)[arg]
            elif op == 107 and arg < len(COMPARISONS):
                detail = COMPARISONS[arg]
            elif op in RELATIVE_JUMPS:
                detail = 'to %d' % (at + arg)
            elif op in ABSOLUTE_JUMPS:
                detail = 'to %d' % arg
        if index >= start:
            line = starts.get(offset)
            print('%5s %5d %-22s %s %s' % ('L%d' % line if line else '', offset,
                                           NAMES.get(op, str(op)), '' if arg is None else arg, detail))
            emitted += 1
        index += 1
    if at < len(code.co_code):
        print('TRUNCATED: next page starts at instruction', index, 'byte', at)


def outline(code, prefix):
    # Every nested code object with its arguments, global/attribute names and literal constants.
    print('CODE:', prefix, 'line', code.co_firstlineno, 'args', code.co_varnames[:code.co_argcount])
    print('  NAMES:', ' '.join(code.co_names))
    literals = [item for item in code.co_consts if type(item).__name__ != 'code' and item is not None]
    if literals:
        print('  CONSTANTS:', repr(literals)[:3000])
    for item in code.co_consts:
        if type(item).__name__ == 'code':
            outline(item, prefix + '.' + item.co_name)


def references(code, prefix):
    values = code.co_names + tuple(item for item in code.co_consts if isinstance(item, basestring))
    matches = [item for item in values if 'override' in item.lower() or item in ('pllCount', 'pixelRate', 'activeW', 'activeH', 'offX')]
    count = 0
    if matches:
        print('MODE_REFERENCE:', prefix, code.co_firstlineno, repr(matches))
        count += 1
    for item in code.co_consts:
        if type(item).__name__ == 'code':
            count += references(item, prefix + '.' + item.co_name)
    return count


def module_code(module):
    # Package members such as usbcon/usbio need an importer rooted at the package folder.
    package, _, name = module.rpartition('/')
    root = config['vendor_dir'] + '\\library.zip'
    return zipimport.zipimporter(root + '\\' + package.replace('/', '\\') if package else root).get_code(name)


probes = config.get('probes') or DEFAULT_PROBES
if not config.get('probes'):
    count = 0
    for module in ('actions', 'wxer', 'adapter'):
        count += references(module_code(module), module)
    print('BOUNDED_TIMING_REFERENCE_SCAN:', count, 'code objects in actions, wxer, adapter; other modules unexamined')
for module, symbol, start in probes:
    if symbol == '*':
        print('STATIC_OUTLINE:', module)
        outline(module_code(module), module)
        continue
    print('STATIC_VIDEO_SETTINGS:', module, symbol or '<module>', 'from instruction', start)
    show(select(module_code(module), [part for part in symbol.split('.') if part]), start)
print('STATIC_INSPECTION_ONLY: no original module imported; no device opened')
