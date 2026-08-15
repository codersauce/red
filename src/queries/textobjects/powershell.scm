; Red-owned structural queries for the bundled PowerShell grammar.

(function_statement
  (script_block) @function.inner) @function.outer

(class_statement) @class.outer

(class_statement
  (class_method_definition) @class.inner)

(class_method_definition) @function.outer

(class_method_definition
  (script_block) @function.inner)

(command) @call.outer

(command
  (command_elements) @call.inner)

(parameter_list
  (_) @parameter.inner)

(class_method_parameter_list
  (class_method_parameter) @parameter.inner)

(comment) @comment.outer
