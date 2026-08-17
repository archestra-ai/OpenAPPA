export default {
  extends: ['@commitlint/config-conventional'],
  rules: {
    'body-leading-blank': [1, 'always'],
    'body-max-line-length': [1, 'always', 100],
    'footer-leading-blank': [1, 'always'],
    'footer-max-line-length': [1, 'always', 100],
    'header-max-length': [1, 'always', 100],
    'subject-case': [0, 'never', ['sentence-case', 'start-case', 'pascal-case', 'upper-case']],
    'subject-empty': [2, 'never'],
    'subject-full-stop': [2, 'never', '.'],
    'type-case': [2, 'always', ['lower-case', 'camel-case']],
    'type-empty': [2, 'never'],
    // Two vocabularies, both accepted. The first names the kind of change and
    // drives release-please's changelog sections. The second names the part of
    // the repository a title is about; those land in the "Other" section.
    // Add a component here when a new crate or top-level area appears.
    'type-enum': [
      2,
      'always',
      [
        // Kind of change.
        'build',
        'chore',
        'ci',
        'deps',
        'docs',
        'feat',
        'fix',
        'perf',
        'refactor',
        'revert',
        'style',
        'test',
        // Part of the repository.
        'appa-agent',
        'appa-runtime',
        'appa-sdk',
        'bench',
        'bench-corp',
        'builtin',
        'demo',
        'engine',
        'eventlog',
        'integrations',
        'playground',
        'policy',
        'repo',
        'runtime',
        'runtime-v2',
        'spec',
        'website',
      ],
    ],
  },
};
