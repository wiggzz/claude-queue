# Changelog

## [0.3.0](https://github.com/wiggzz/claude-queue/compare/v0.2.0...v0.3.0) (2026-03-12)


### Features

* add --verbose flag to cq audit for full tool call details ([#12](https://github.com/wiggzz/claude-queue/issues/12)) ([8c962f9](https://github.com/wiggzz/claude-queue/commit/8c962f9d385ff2d1b2560023dfdff64fdb788e5e))
* derive cq policies from Claude Code permission settings ([#7](https://github.com/wiggzz/claude-queue/issues/7)) ([fe99dc7](https://github.com/wiggzz/claude-queue/commit/fe99dc7e937dd3e069b71d9a96fa5009a37fcec0))


### Bug Fixes

* concatenate outputs from resumed sessions in cq result ([#10](https://github.com/wiggzz/claude-queue/issues/10)) ([344e8f6](https://github.com/wiggzz/claude-queue/commit/344e8f6a512cb267cb3109e8c1d4a7071a1075b4))
* omit agent session prompt from supervisor context by default ([#6](https://github.com/wiggzz/claude-queue/issues/6)) ([739e6e8](https://github.com/wiggzz/claude-queue/commit/739e6e82b0c3d44e4141d64a97f3cb0259b07e64))
* resolve project root from git worktrees for config loading ([#8](https://github.com/wiggzz/claude-queue/issues/8)) ([60b61af](https://github.com/wiggzz/claude-queue/commit/60b61afa438ee33a2d1115012e505b8af690be68))
* use RELEASE_PLEASE_TOKEN for release-please action ([#15](https://github.com/wiggzz/claude-queue/issues/15)) ([7863679](https://github.com/wiggzz/claude-queue/commit/7863679521f8757f7d8603c3a6017db12254f846))

## [0.2.0](https://github.com/wiggzz/claude-queue/compare/v0.1.0...v0.2.0) (2026-03-12)


### Features

* add `cq version` subcommand ([bdbd56e](https://github.com/wiggzz/claude-queue/commit/bdbd56e0aa6322530ade15265a814386a18b3935))
* enable supervisor by default with sensible system prompt ([2a840a6](https://github.com/wiggzz/claude-queue/commit/2a840a6b745d3c082265462a809844638d8fe9de))


### Bug Fixes

* install script creates target directory if missing ([6b845e3](https://github.com/wiggzz/claude-queue/commit/6b845e322a98c784a034bcbc8bbb97e1c8cc3427))
* run CI on release-please branches ([#5](https://github.com/wiggzz/claude-queue/issues/5)) ([c600fc8](https://github.com/wiggzz/claude-queue/commit/c600fc894a3f4ff90280454f9d26467141de602e))
* trigger CI on all pull requests and merge groups ([#3](https://github.com/wiggzz/claude-queue/issues/3)) ([c6d6b33](https://github.com/wiggzz/claude-queue/commit/c6d6b33ee39d5e512e6ade60c2d638c01a17ea2f))

## 0.1.0 (2026-03-12)


### Features

* add supervisor summaries, self-update, and audit improvements ([12a4da3](https://github.com/wiggzz/claude-queue/commit/12a4da3e16b9af5656f1f17114cc158e5aa934ac))


### Bug Fixes

* correct hook output format and make supervisor escalate-only ([684dbe7](https://github.com/wiggzz/claude-queue/commit/684dbe7ef720be81023b69130ff17ed9b442b82c))
