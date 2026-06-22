# Changelog

## [0.10.0](https://github.com/ornitech/rumor/compare/v0.9.0...v0.10.0) (2026-06-22)


### Features

* configurable per-process retry/auto-restart ([#26](https://github.com/ornitech/rumor/issues/26)) ([e388578](https://github.com/ornitech/rumor/commit/e388578445855686cd836ce8c48d34acfb407261))
* **docs:** add 'rumor docs --agent' reference and --help flag ([#29](https://github.com/ornitech/rumor/issues/29)) ([3c0d4d8](https://github.com/ornitech/rumor/commit/3c0d4d8ca49e1f4b4ea67df0df888a7c39893d8a))
* **raw:** add single-stream output mode for AI agents ([#28](https://github.com/ornitech/rumor/issues/28)) ([8459379](https://github.com/ornitech/rumor/commit/84593799d03310e988bc47566c2873575dd8a8af))

## [0.9.0](https://github.com/ornitech/rumor/compare/v0.8.0...v0.9.0) (2026-06-10)


### Features

* **config:** dynamic per-worktree port allocation via dynamicPorts ([#24](https://github.com/ornitech/rumor/issues/24)) ([ebe2f2e](https://github.com/ornitech/rumor/commit/ebe2f2e862777ca09cf694e7af6aa1df8752e103))
* **logs:** always-on session log capture with copyable log paths ([#21](https://github.com/ornitech/rumor/issues/21)) ([da48695](https://github.com/ornitech/rumor/commit/da48695d73db4680e8eeda8e92acd2b29b5d7109))
* **ui:** hotkey help overlay on h ([#23](https://github.com/ornitech/rumor/issues/23)) ([4b63a74](https://github.com/ornitech/rumor/commit/4b63a74377b93759951b1360a124def5e7d1fa15))
* **ui:** interactive search in log view and details pane ([#25](https://github.com/ornitech/rumor/issues/25)) ([7d4edc6](https://github.com/ornitech/rumor/commit/7d4edc68619d953e3fb2fdb2cb72e153b9296523))

## [0.8.0](https://github.com/ornitech/rumor/compare/v0.7.0...v0.8.0) (2026-06-08)


### Features

* **config:** run a subset of processes with -t/--tags ([#19](https://github.com/ornitech/rumor/issues/19)) ([3b68169](https://github.com/ornitech/rumor/commit/3b68169b7786b01439f1b9f0dbe702b06a846b10))

## [0.7.0](https://github.com/ornitech/rumor/compare/v0.6.0...v0.7.0) (2026-06-05)


### Features

* **config:** default to ./rumor.json when no path is given ([#17](https://github.com/ornitech/rumor/issues/17)) ([177f22f](https://github.com/ornitech/rumor/commit/177f22fb2add4577eeac70dde1849ffa9b277dbc))

## [0.6.0](https://github.com/ornitech/rumor/compare/v0.5.0...v0.6.0) (2026-06-04)


### Features

* **ui:** show background shutdown progress on quit ([#15](https://github.com/ornitech/rumor/issues/15)) ([d832bb8](https://github.com/ornitech/rumor/commit/d832bb860429b24246428636a370c202209dc265))

## [0.5.0](https://github.com/ornitech/rumor/compare/v0.4.0...v0.5.0) (2026-05-29)


### Features

* **config:** add root-level envFiles shared by all processes ([#11](https://github.com/ornitech/rumor/issues/11)) ([3a65377](https://github.com/ornitech/rumor/commit/3a65377d01799b8ee19acf8d497573722a95e5a8))


### Bug Fixes

* **ui:** spawn restarted processes at the current terminal width ([#13](https://github.com/ornitech/rumor/issues/13)) ([7659cdb](https://github.com/ornitech/rumor/commit/7659cdb39c34fb0fe756fdcf50f95a79e25cc866))

## [0.4.0](https://github.com/ornitech/rumor/compare/v0.3.0...v0.4.0) (2026-05-27)


### Features

* **ui:** add 'd' hotkey for process details screen ([#9](https://github.com/ornitech/rumor/issues/9)) ([3e76fd6](https://github.com/ornitech/rumor/commit/3e76fd6fe7e901e91197710d6414eaf5dab14a7d))

## [0.3.0](https://github.com/ornitech/rumor/compare/v0.2.0...v0.3.0) (2026-05-27)


### Features

* color status dots by exit code and process kind ([#5](https://github.com/ornitech/rumor/issues/5)) ([16b2115](https://github.com/ornitech/rumor/commit/16b21150153a343fa18abe285e7e982d1ed805ca))
* **config:** substitute ${VAR} refs against per-process env ([#6](https://github.com/ornitech/rumor/issues/6)) ([fa7b1f2](https://github.com/ornitech/rumor/commit/fa7b1f2438b8dd1e4b66d066101c39f0124f0adc))

## [0.2.0](https://github.com/ornitech/rumor/compare/rumor-v0.1.0...rumor-v0.2.0) (2026-05-25)


### Features

* hanoi multi-process TUI orchestrator ([#1](https://github.com/ornitech/rumor/issues/1)) ([f82696c](https://github.com/ornitech/rumor/commit/f82696cc9a17100cd210e037372c56c12c2f389c))
