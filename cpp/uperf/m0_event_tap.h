/*
 * Copyright (C) 2026 grill-glitch
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// M0-ONLY harness: subscribes to every topic the reused platform publishes and logs the
// events, so that M0 can prove the vendored event pipeline end to end on a real device.
//
// This file is deleted at M1, when the Rust side takes over as the subscriber
// (cpp/include/uperf_rs.h / uperf_rs_on_event). It is also what activates
// CgroupListener's updaters: they only register work when the topic has a subscriber.

#pragma once

#include "platform/module_base.h"
#include <cstdint>
#include <string>

class M0EventTap : public ModuleBase {
public:
    M0EventTap();
    ~M0EventTap() override;
    void Start(void) override;

private:
    int64_t events_;

    void OnTouch(const void *data);
    void OnBtn(const void *data);
    void OnInputState(const void *data);
    void OnTopapp(const void *data);
    void OnOffscreen(const void *data);
    void OnCgroupList(const char *topic, const void *data);
    void OnCgroupUpdate(const char *topic);
};
