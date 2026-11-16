(emphasis) @emphasis
(strong_emphasis) @strong_emphasis
(strikethrough) @strikethrough

(code_span) @code
(code_span_delimiter) @code_delimiter

[(link_text) (link_label) (image_description)] @link
[(link_destination) (uri_autolink) (email_autolink)] @link_target
(link_title) @string
(inline_link ["[" "]" "(" ")"] @separator)
(full_reference_link ["[" "]"] @separator)
(collapsed_reference_link ["[" "]"] @separator)
(shortcut_link ["[" "]"] @separator)
(link_label ["[" "]"] @separator)
(image ["!" "[" "]" "(" ")"] @separator)

((html_tag) @comment (#match? @comment "^<!--"))
(html_tag) @tag
[(entity_reference) (numeric_character_reference)] @constant
[(backslash_escape) (hard_line_break)] @string
