(atx_heading (inline) @text.title)
(setext_heading (paragraph) @text.title)
(pipe_table_header (pipe_table_cell) @text.title)
[
  (atx_h1_marker) (atx_h2_marker) (atx_h3_marker) (atx_h4_marker) (atx_h5_marker) (atx_h6_marker)
  (setext_h1_underline) (setext_h2_underline)
  (list_marker_plus) (list_marker_minus) (list_marker_star)
  (list_marker_dot) (list_marker_parenthesis) (thematic_break)
  (block_quote_marker) (block_continuation)
] @punctuation.special
[(indented_code_block) (info_string)] @text.literal
(fenced_code_block_delimiter) @punctuation.delimiter
[(link_label) (link_destination)] @text.reference
