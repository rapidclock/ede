[
 "abstract" "as" "async" "await" "break" "case" "catch" "class" "const"
 "continue" "debugger" "declare" "default" "delete" "do" "else" "export" "extends"
 "finally" "for" "from" "function" "get" "if" "import" "in" "instanceof"
 "interface" "let" "namespace" "new" "of" "private" "protected" "public" "readonly"
 "return" "set" "static" "switch" "throw" "try" "type" "typeof" "var" "void"
 "while" "with" "yield"
] @keyword
(type_identifier) @type
(predefined_type) @type
(property_identifier) @property
(function_declaration name: (identifier) @function)
[(string) (template_string)] @string
(number) @number
[(true) (false) (null) (undefined)] @constant
(comment) @comment
(escape_sequence) @string.escape
(template_substitution [ "${" "}" ] @punctuation.special)
[ "+" "-" "*" "/" "%" "=" "===" "!==" "&&" "||" "??" "?" ":" ] @operator
