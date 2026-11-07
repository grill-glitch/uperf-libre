/*
 * Copyright (C) 2021-2022 Matt Yang
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

#include "input_reader.h"
#include "fmt_exception.h"
#include "misc.h"
#include <cstring>
#include <fcntl.h>
#include <linux/input.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

enum KeyState { KEY_STATE_UP = 0, KEY_STATE_DOWN = 1 };

// EV_KEY carries 0 (release), 1 (press) and 2 (autorepeat). A repeat is not a release:
// treating it as one flipped `pressed` to false on every repeat of a held button.
static bool KeyPressed(int value, bool prev) {
    if (value == KEY_STATE_UP) {
        return false;
    }
    if (value == KEY_STATE_DOWN) {
        return true;
    }
    return prev;
}

constexpr std::size_t INPUT_MAX_EVENTS = 1024;

InputReader::InputReader(const std::string &path) : buf_(sizeof(input_event) * INPUT_MAX_EVENTS) {
    prop_.fd = open(path.c_str(), O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    prop_.path = path;
    if (prop_.fd < 0) {
        throw FmtException("Cannot open input device {}", path);
    }

    // Any throw from the *_Prop readers (e.g. std::bad_alloc while assigning a name) would
    // otherwise exit the constructor without running the destructor and leak the fd.
    try {
        if (IsTouchPanel()) {
            ReadTouchpanelProp();
        } else if (IsMouseDev()) {
            ReadMouseProp();
        } else if (IsBtnDev()) {
            ReadBtnProp();
        } else {
            throw FmtException("Unknown input device {}", path);
        }
    } catch (...) {
        close(prop_.fd);
        prop_.fd = -1;
        throw;
    }
}

InputReader::~InputReader() {
    if (prop_.fd >= 0) {
        close(prop_.fd);
        prop_.fd = -1;
    }
}

const InputReader::DevProp &InputReader::GetProp(void) const { return prop_; }

// File-local: a generic name at global scope can collide with another translation unit.
static float LimitRange(int val, const std::pair<int, int> &range) {
    return static_cast<float>(val - range.first) / (range.second - range.first + 1);
}

void InputReader::Parse(const InfoHandler &handler) {
    info_.ts = GetNowTs();
    auto len = read(prop_.fd, buf_.data(), buf_.size());
    if (len <= 0) {
        return;
    }
    // A partial trailing event (short read) would be reinterpreted as a whole one.
    len -= len % static_cast<ssize_t>(sizeof(input_event));
    if (len <= 0) {
        return;
    }

    auto nr = len / sizeof(input_event);
    // `buf_` is a byte vector: casting its storage to `input_event *` is both an alignment
    // and an object-lifetime violation. Copy each event into a properly typed local.
    for (size_t i = 0; i < nr; i++) {
        input_event evt;
        memcpy(&evt, buf_.data() + i * sizeof(input_event), sizeof(evt));
        switch (prop_.devType) {
            case DevType::TOUCH_PANEL:
                ParseTouchPanel(&evt, handler);
                break;
            case DevType::BTN:
                ParseBtn(&evt, handler);
                break;
            case DevType::MOUSE:
                ParseMouse(&evt, handler);
                break;
            default:
                break;
        }
    }
}

bool InputReader::IsTouchPanel(void) {
    auto fd = prop_.fd;

    // support absolute position?
    unsigned long evbit = 0;
    // A failed ioctl leaves evbit 0, which would classify the device as "not a touch panel"
    // — a transient failure must not silently misclassify it.
    if (ioctl(fd, EVIOCGBIT(0, sizeof(evbit)), &evbit) < 0) {
        return false;
    }
    bool hasAbsFeature = evbit & (1 << EV_ABS);
    if (hasAbsFeature == false) {
        return false;
    }

    // legal absolute position range?
    struct input_absinfo absinfo;
    memset(&absinfo, 0, sizeof(absinfo));
    ioctl(fd, EVIOCGABS(ABS_MT_POSITION_X), &absinfo);
    if (absinfo.minimum == absinfo.maximum) {
        return false;
    }
    memset(&absinfo, 0, sizeof(absinfo));
    ioctl(fd, EVIOCGABS(ABS_MT_POSITION_Y), &absinfo);
    if (absinfo.minimum == absinfo.maximum) {
        return false;
    }

    return true;
}

bool InputReader::IsBtnDev(void) {
    auto fd = prop_.fd;

    // support key?
    unsigned long evbit = 0;
    if (ioctl(fd, EVIOCGBIT(0, sizeof(evbit)), &evbit) < 0) {
        return false;
    }
    bool hasKeyFeature = evbit & (1 << EV_KEY);
    if (hasKeyFeature == false) {
        return false;
    }

    return true;
}

bool InputReader::IsMouseDev(void) {
    auto fd = prop_.fd;

    // support relative position?
    unsigned long evbit = 0;
    if (ioctl(fd, EVIOCGBIT(0, sizeof(evbit)), &evbit) < 0) {
        return false;
    }
    bool hasRelFeature = evbit & (1 << EV_REL);
    if (hasRelFeature == false) {
        return false;
    }

    return true;
}

void InputReader::ReadTouchpanelProp() {
    auto fd = prop_.fd;
    prop_.devType = DevType::TOUCH_PANEL;

    char name[64] = {0};
    if (ioctl(fd, EVIOCGNAME(sizeof(name)), name) < 0) {
        prop_.name = "?";
    } else {
        prop_.name = name;
    }

    // Zeroed first: a failed ioctl must not leave the coordinate ranges at whatever was on
    // the stack, and a range of 0..0 makes LimitRange divide by 1 instead of 0.
    struct input_absinfo absinfo;
    memset(&absinfo, 0, sizeof(absinfo));
    if (ioctl(fd, EVIOCGABS(ABS_MT_POSITION_X), &absinfo) == 0) {
        prop_.xRange.first = absinfo.minimum;
        prop_.xRange.second = absinfo.maximum;
    }
    memset(&absinfo, 0, sizeof(absinfo));
    if (ioctl(fd, EVIOCGABS(ABS_MT_POSITION_Y), &absinfo) == 0) {
        prop_.yRange.first = absinfo.minimum;
        prop_.yRange.second = absinfo.maximum;
    }
}

void InputReader::ReadBtnProp() {
    auto fd = prop_.fd;
    prop_.devType = DevType::BTN;

    char name[64] = {0};
    prop_.name = ioctl(fd, EVIOCGNAME(sizeof(name)), name) < 0 ? "?" : name;
}

void InputReader::ReadMouseProp() {
    auto fd = prop_.fd;
    prop_.devType = DevType::MOUSE;

    char name[64] = {0};
    prop_.name = ioctl(fd, EVIOCGNAME(sizeof(name)), name) < 0 ? "?" : name;
}

void InputReader::ParseTouchPanel(const input_event *evt, const InfoHandler &handler) {
    switch (evt->type) {
        case EV_SYN:
            if (handler) {
                handler(info_);
            }
            break;
        case EV_ABS:
            switch (evt->code) {
                case ABS_MT_POSITION_X:
                    info_.x = LimitRange(evt->value, prop_.xRange);
                    break;
                case ABS_MT_POSITION_Y:
                    info_.y = LimitRange(evt->value, prop_.yRange);
                    break;
                case ABS_MT_TRACKING_ID:
                    if (prop_.useEvtKey == false) {
                        info_.pressed = (evt->value != UINT_MAX);
                    }
                    break;
                default:
                    break;
            }
            break;
        case EV_KEY:
            switch (evt->code) {
                case BTN_TOUCH:
                case BTN_TOOL_FINGER:
                    info_.pressed = KeyPressed(evt->value, info_.pressed);
                    prop_.useEvtKey = true;
                    break;
                default:
                    break;
            }
            break;
        default:
            break;
    }
}

void InputReader::ParseBtn(const input_event *evt, const InfoHandler &handler) {
    switch (evt->type) {
        case EV_SYN:
            if (handler) {
                handler(info_);
            }
            break;
        case EV_KEY:
            info_.pressed = KeyPressed(evt->value, info_.pressed);
            break;
        default:
            break;
    }
}

void InputReader::ParseMouse(const input_event *evt, const InfoHandler &handler) {
    // ignore mouse movement which contains EV_REL, EV_SYN
    switch (evt->type) {
        case EV_KEY:
            info_.pressed = KeyPressed(evt->value, info_.pressed);
            if (handler) {
                handler(info_);
            }
            break;
        default:
            break;
    }
}

const char *DevTypeName(InputReader::DevType type) {
    switch (type) {
        case InputReader::DevType::TOUCH_PANEL:
            return "touchpanel";
        case InputReader::DevType::BTN:
            return "btn";
        case InputReader::DevType::MOUSE:
            return "mouse";
        default:
            break;
    }
    return "unknown";
}
