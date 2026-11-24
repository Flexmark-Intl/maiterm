import { describe, expect, it } from 'vitest';
import {
  checkCreateTasksArgs,
  checkDeleteTasksArgs,
  checkListTasksArgs,
  checkUpdateTasksArgs,
  resolveCreateTaskWorkstream,
  validateOptionalStringArg,
  validateStringArrayArg,
} from './toolArgs';

describe('validateOptionalStringArg', () => {
  it('accepts undefined, strings, and null when allowNull is true', () => {
    expect(validateOptionalStringArg(undefined, 'note')).toBeNull();
    expect(validateOptionalStringArg('ok', 'note')).toBeNull();
    expect(validateOptionalStringArg('', 'workstream', { allowNull: true })).toBeNull();
    expect(validateOptionalStringArg(null, 'workstream', { allowNull: true })).toBeNull();
  });

  it('rejects non-strings (and null unless allowNull)', () => {
    expect(validateOptionalStringArg(123, 'title')).toBe('title must be a string.');
    expect(validateOptionalStringArg(['a', 'b'], 'note')).toBe('note must be a string.');
    expect(validateOptionalStringArg(null, 'status')).toBe('status must be a string.');
    expect(validateOptionalStringArg(42, 'workstream', { allowNull: true })).toBe(
      'workstream must be a string (or null).',
    );
  });
});

describe('validateStringArrayArg', () => {
  it('accepts undefined and string arrays (including empty)', () => {
    expect(validateStringArrayArg(undefined, 'status')).toBeNull();
    expect(validateStringArrayArg([], 'status')).toBeNull();
    expect(validateStringArrayArg(['active', 'blocked'], 'status')).toBeNull();
  });

  it('rejects bare strings, null, non-arrays, and arrays containing non-strings', () => {
    expect(validateStringArrayArg('active', 'status', '["active"]')).toBe(
      'status must be an array of strings, e.g. ["active"].',
    );
    expect(validateStringArrayArg(null, 'ids')).toBe('ids must be an array of strings.');
    expect(validateStringArrayArg(123, 'workstreams')).toBe('workstreams must be an array of strings.');
    expect(validateStringArrayArg(['ok', 42], 'blocked_by')).toBe('blocked_by must be an array of strings.');
  });
});

describe('resolveCreateTaskWorkstream', () => {
  it('inherits the call-level workstream when the task omits it', () => {
    expect(resolveCreateTaskWorkstream(undefined, 'Auth refactor')).toBe('Auth refactor');
    expect(resolveCreateTaskWorkstream(undefined, '  Auth refactor  ')).toBe('Auth refactor');
    expect(resolveCreateTaskWorkstream(undefined, undefined)).toBeNull();
    expect(resolveCreateTaskWorkstream(undefined, '   ')).toBeNull();
  });

  it('lets a per-task workstream win over the call-level workstream', () => {
    expect(resolveCreateTaskWorkstream('Hive on Antigravity', undefined)).toBe('Hive on Antigravity');
    expect(resolveCreateTaskWorkstream('DB migration', 'Auth refactor')).toBe('DB migration');
  });

  it('lets an empty string or null override the call-level workstream to loose', () => {
    expect(resolveCreateTaskWorkstream('', 'Auth refactor')).toBeNull();
    expect(resolveCreateTaskWorkstream('   ', 'Auth refactor')).toBeNull();
    expect(resolveCreateTaskWorkstream(null, 'Auth refactor')).toBeNull();
  });
});

describe('checkCreateTasksArgs', () => {
  it('emits no warnings for valid arguments including per-task workstream and auto-injected tabId', () => {
    expect(
      checkCreateTasksArgs({
        tabId: 'tab-1',
        workstream: 'Default job',
        tasks: [
          {
            title: 'Ship the fix',
            detail: 'Details here',
            workstream: 'Hive on Antigravity',
            status: 'todo',
            blocked_by: ['id-1'],
            assign_to_me: true,
            blocker: {
              kind: 'decision',
              question: 'Which approach?',
              context: 'Tradeoffs',
              pick: 'one',
              options: [{ label: 'A', detail: 'Fast', recommended: true }],
            },
          },
        ],
      }),
    ).toEqual([]);
  });

  it('warns on unknown fields like `notes` inside a task item while accepting per-task `workstream`', () => {
    // The exact bug reported by the Hive tab: `workstream` + `notes` on each task.
    const warnings = checkCreateTasksArgs({
      tasks: [
        { title: 'Task 1', workstream: 'Hive on Antigravity', notes: 'Long explanation' },
        { title: 'Task 2', workstream: 'Hive on Antigravity', description: 'Another body' },
      ],
    });
    expect(warnings).toEqual([
      'tasks[0]: ignored unknown key "notes" (did you mean "detail"?)',
      'tasks[1]: ignored unknown key "description" (did you mean "detail"?)',
    ]);
  });

  it('suggests fixes for camelCase, typos, and misplaced top-level keys', () => {
    const warnings = checkCreateTasksArgs({
      title: 'Put at top level by mistake',
      tasks: [
        {
          title: 'Real task',
          blockedBy: ['id-1'],
          worksteam: 'Typo',
          priority: 'high',
        },
      ],
    });
    expect(warnings).toEqual([
      'ignored unknown key "title" (belongs inside each item of "tasks")',
      'tasks[0]: ignored unknown key "blockedBy" (did you mean "blocked_by"?)',
      'tasks[0]: ignored unknown key "worksteam" (did you mean "workstream"?)',
      'tasks[0]: ignored unknown key "priority"',
    ]);
  });

  it('warns on unknown keys inside nested blocker and blocker.options objects', () => {
    const warnings = checkCreateTasksArgs({
      tasks: [
        {
          title: 'Ask human',
          blocker: {
            type: 'decision',
            question: 'Pick one?',
            runIn: 'peer-1',
            options: [{ title: 'Option A', description: 'Costs less', default: true }],
          },
        },
      ],
    });
    expect(warnings).toEqual([
      'tasks[0].blocker: ignored unknown key "type" (did you mean "kind"?)',
      'tasks[0].blocker: ignored unknown key "runIn" (did you mean "run_in"?)',
      'tasks[0].blocker.options[0]: ignored unknown key "title" (did you mean "label"?)',
      'tasks[0].blocker.options[0]: ignored unknown key "description" (did you mean "detail"?)',
      'tasks[0].blocker.options[0]: ignored unknown key "default" (did you mean "recommended"?)',
    ]);
  });

  it('reports skipped items when mixed with valid ones, keeping true caller indices', () => {
    const warnings = checkCreateTasksArgs(
      {
        tasks: [
          { name: 'Forgot title key' },
          'bare string',
          { title: 'Valid task', notes: 'Oops' },
        ],
      },
      { reportSkipped: true },
    );
    expect(warnings).toEqual([
      'tasks[0]: ignored unknown key "name" (did you mean "title"?)',
      'tasks[0]: skipped (missing required "title")',
      'tasks[1]: skipped (expected an object with "title")',
      'tasks[2]: ignored unknown key "notes" (did you mean "detail"?)',
    ]);
  });

  it('ignores keys whose value is undefined', () => {
    expect(
      checkCreateTasksArgs({
        tasks: [{ title: 'Valid', notes: undefined }],
      }),
    ).toEqual([]);
  });

  it('never leaks Object.prototype properties as hints and normalizes camelCase/kebab-case aliases', () => {
    expect(
      checkCreateTasksArgs(JSON.parse('{"__proto__": 1, "tasks": [{"title": "x", "constructor": 1, "toString": 1, "blockOn": ["id-1"], "assign-to": "me"}]}')),
    ).toEqual([
      'ignored unknown key "__proto__"',
      'tasks[0]: ignored unknown key "constructor"',
      'tasks[0]: ignored unknown key "toString"',
      'tasks[0]: ignored unknown key "blockOn" (did you mean "blocked_by"?)',
      'tasks[0]: ignored unknown key "assign-to" (did you mean "assign_to_me"? (boolean; use updateTasks "assign_to" to assign to another tab))',
    ]);
  });
});

describe('checkUpdateTasksArgs', () => {
  it('emits no warnings for valid updates including blocker: null', () => {
    expect(
      checkUpdateTasksArgs({
        tabId: 'tab-1',
        updates: [
          {
            id: 't1',
            status: 'active',
            title: 'Renamed',
            detail: 'Spec',
            note: 'Progress',
            workstream: 'Job',
            blocked_by: [],
            block_on: ['t0'],
            unblock_from: [],
            blocker: null,
            assign_to: 'me',
          },
        ],
      }),
    ).toEqual([]);
  });

  it('warns on unknown keys in top-level, item, and nested blocker', () => {
    const warnings = checkUpdateTasksArgs(
      {
        tasks: [],
        updates: [
          { task_id: 't1', status: 'done' },
          {
            id: 't2',
            notes: 'Progress log',
            assignee: 'me',
            blocker: { kind: 'decision', prompt: 'Which?', choices: [] },
          },
        ],
      },
      { reportSkipped: true },
    );
    expect(warnings).toEqual([
      'ignored unknown key "tasks" (did you mean "updates"?)',
      'updates[0]: ignored unknown key "task_id" (did you mean "id"?)',
      'updates[0]: skipped (missing required "id")',
      'updates[1]: ignored unknown key "notes" (did you mean "note" (append to progress log) or "detail" (replace body)?)',
      'updates[1]: ignored unknown key "assignee" (did you mean "assign_to"?)',
      'updates[1].blocker: ignored unknown key "prompt" (did you mean "question"?)',
      'updates[1].blocker: ignored unknown key "choices" (did you mean "options"?)',
    ]);
  });
});

describe('checkListTasksArgs', () => {
  it('emits no warnings for valid arguments', () => {
    expect(
      checkListTasksArgs({ tabId: 'tab-1', scope: 'tab', status: ['active'], ready: false, limit: 50 }),
    ).toEqual([]);
  });

  it('warns on unknown keys and does not suggest tabId for unrelated words', () => {
    const warnings = checkListTasksArgs({
      workstream: 'Auth',
      statuses: ['todo'],
      tab_id: 'tab-1',
      max: 10,
      table: 'foo',
    });
    expect(warnings).toEqual([
      'ignored unknown key "workstream" (listTasks returns all workstreams grouped; it does not filter by workstream)',
      'ignored unknown key "statuses" (did you mean "status"?)',
      'ignored unknown key "tab_id" (did you mean "scope": "tab"?)',
      'ignored unknown key "max" (did you mean "limit"?)',
      'ignored unknown key "table"',
    ]);
  });
});

describe('checkDeleteTasksArgs', () => {
  it('emits no warnings for valid arguments', () => {
    expect(checkDeleteTasksArgs({ tabId: 'tab-1', ids: ['t1'], workstreams: ['Old'] })).toEqual([]);
  });

  it('warns on singular/alias keys like id and workstream', () => {
    expect(checkDeleteTasksArgs({ id: 't1', workstream: 'Old', force: true })).toEqual([
      'ignored unknown key "id" (did you mean "ids"?)',
      'ignored unknown key "workstream" (did you mean "workstreams"?)',
      'ignored unknown key "force"',
    ]);
  });
});
