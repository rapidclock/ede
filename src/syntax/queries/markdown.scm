(atx_heading (atx_h1_marker)) @heading_1
(atx_heading (atx_h2_marker)) @heading_2
(atx_heading (atx_h3_marker)) @heading_3
(atx_heading (atx_h4_marker)) @heading_4
(atx_heading (atx_h5_marker)) @heading_5
(atx_heading (atx_h6_marker)) @heading_6
(setext_heading (setext_h1_underline)) @heading_1
(setext_heading (setext_h2_underline)) @heading_2
(pipe_table_header (pipe_table_cell) @text.title)
[
  (list_marker_plus) (list_marker_minus) (list_marker_star)
  (list_marker_dot) (list_marker_parenthesis)
] @list_marker
[
  (thematic_break)
  (block_quote_marker) (block_continuation)
] @punctuation.special
[(indented_code_block) (info_string)] @text.literal
(fenced_code_block_delimiter) @punctuation.delimiter
[(link_label) (link_destination)] @text.reference
