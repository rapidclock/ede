(atx_heading (atx_h1_marker)) @heading_1
(atx_heading (atx_h2_marker)) @heading_2
(atx_heading (atx_h3_marker)) @heading_3
(atx_heading (atx_h4_marker)) @heading_4
(atx_heading (atx_h5_marker)) @heading_5
(atx_heading (atx_h6_marker)) @heading_6
(setext_heading (setext_h1_underline)) @heading_1
(setext_heading (setext_h2_underline)) @heading_2
(pipe_table_header (pipe_table_cell) @table_header)
[
  (list_marker_plus) (list_marker_minus) (list_marker_star)
] @list_marker
[(list_marker_dot) (list_marker_parenthesis)] @ordered_list_marker
(task_list_marker_checked) @task_checked
(task_list_marker_unchecked) @task_unchecked

(block_quote_marker) @quote_marker
((block_continuation) @quote_marker (#match? @quote_marker ">"))
(thematic_break) @separator
(pipe_table_header "|" @separator)
(pipe_table_row "|" @separator)
(pipe_table_delimiter_row) @separator

(indented_code_block) @code
(fenced_code_block_delimiter) @code_delimiter
(info_string (language) @fence_language)
((html_block) @comment (#match? @comment "^[ \t]*<!--"))

(link_label) @link
(link_destination) @link_target
(link_title) @string
(link_reference_definition ":" @separator)
(link_label ["[" "]"] @separator)
[(entity_reference) (numeric_character_reference)] @constant
(backslash_escape) @string
