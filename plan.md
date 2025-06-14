Refactoring Plan: Eliminating Action Execution Duplication in Red Editor

Overview

This plan outlines the steps needed to complete the refactoring of the Red editor's action execution system, eliminating code duplication between production and test code while fixing discovered issues.

Current State

- Partially implemented apply_action_core method that separates core logic from side effects
- 16/24 editing tests passing (down from 24/24 due to refactoring exposing bugs)
- Production execute method still contains mixed logic and side effects
- Several buffer implementation bugs discovered

Phase 1: Fix Critical Bugs (Priority: High)

1.1 Fix arithmetic overflow in buffer.rs

- Issue: find_next_word has arithmetic overflow at line 345
- Root cause: self.len() - 1 can underflow when buffer is empty
- Fix: Use saturating_sub or check for empty buffer first

  1.2 Fix other buffer edge cases

- Review all buffer methods for similar arithmetic issues
- Add boundary checks for empty buffers
- Ensure consistent behavior with vim

Phase 2: Complete Core Action Migration (Priority: High)

2.1 Migrate remaining actions to apply_action_core

Currently missing actions include:

- File operations (Save, SaveAs, OpenFile)
- Search actions (Search, FindNext, FindPrev)
- Visual mode actions
- Complex editing actions (Change, Replace)
- Buffer management (NextBuffer, PrevBuffer)
- LSP actions (GotoDefinition, ShowHover, etc.)

  2.2 Refactor production execute method

- Replace inline action logic with calls to apply_action_core
- Handle side effects based on returned flags
- Maintain backward compatibility

Phase 3: Fix Failing Tests (Priority: High)

3.1 Debug and fix failing tests

Currently failing:

- test_delete_word - buffer method issue
- test_change_word - depends on delete_word
- test_delete_char - MoveToNextWord overflow
- test_insert_at_line_start - MoveToLineStart issue
- test_open_line_above - cursor positioning
- test_delete_to_end_of_line - delete logic
- test_change_to_end_of_line - depends on delete
- test_paste - clipboard implementation

  3.2 Update test expectations

- Some tests may have incorrect expectations
- Verify behavior matches vim
- Update tests to match correct behavior

Phase 4: Improve Architecture (Priority: Medium)

4.1 Create action categories

Group actions by type for better organization:
enum ActionCategory {
Movement, // cursor movement
Editing, // text modification
Mode, // mode changes
File, // file operations
View, // viewport/display
Search, // search/replace
Lsp, // language server
Clipboard, // yank/paste
Undo, // undo/redo
}

4.2 Implement action traits

trait ActionHandler {
fn category(&self) -> ActionCategory;
fn execute_core(&self, editor: &mut Editor) -> ActionResult;
fn needs_render(&self) -> bool;
fn needs_lsp_notify(&self) -> bool;
}

Phase 5: Enhanced Testing (Priority: Medium)

5.1 Add property-based tests

- Use proptest/quickcheck for edge cases
- Test action sequences
- Verify invariants (cursor always in bounds, etc.)

  5.2 Add integration test coverage

- Test real vim compatibility
- Test LSP integration
- Test plugin system interaction

Phase 6: Documentation (Priority: Low)

6.1 Document the new architecture

- Add module documentation
- Document the action execution flow
- Create diagrams showing the separation of concerns

  6.2 Migration guide

- Document how to add new actions
- Best practices for action implementation
- Testing guidelines

Implementation Order

1. Week 1: Fix critical bugs (Phase 1)

- Fix buffer arithmetic issues
- Get all tests passing again

2. Week 2: Complete core migration (Phase 2)

- Migrate all remaining actions
- Refactor production execute method

3. Week 3: Architecture improvements (Phase 4)

- Implement action categories
- Create trait-based system

4. Week 4: Testing and documentation (Phases 5-6)

- Add comprehensive tests
- Document the system

Success Criteria

1. Zero code duplication between test and production
2. All tests passing
3. No arithmetic panics or overflows
4. Clear separation between core logic and side effects
5. Easy to add new actions
6. Well-documented architecture

Risks and Mitigations

1. Risk: Breaking existing functionality

- Mitigation: Incremental migration, comprehensive testing

2. Risk: Performance regression

- Mitigation: Benchmark critical paths, optimize hot spots

3. Risk: Complex actions difficult to migrate

- Mitigation: Start with simple actions, learn patterns

Alternative Approaches Considered

1. Command pattern with objects: More flexible but more complex
2. Macro-based code generation: Less duplication but harder to debug
3. Keep duplication: Simpler but maintenance nightmare

Conclusion

This refactoring will significantly improve code maintainability, reduce bugs, and make it easier to add new features. The phased approach allows for incremental progress while maintaining a working system.
