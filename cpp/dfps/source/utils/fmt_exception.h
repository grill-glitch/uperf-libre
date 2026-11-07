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

#pragma once

#include <exception>
#include <spdlog/fmt/fmt.h>
#include <string>
#include <utility>

class FmtException : public std::exception {
public:
    /// `explicit` so a stray `const char *` cannot be converted into an exception object.
    /// The message is formatted best-effort: fmt::format allocates and can throw, and a
    /// throw while constructing an exception (during unwinding, or in a noexcept context)
    /// would call std::terminate.
    template <typename... Args>
    explicit FmtException(fmt::format_string<Args...> fmt, Args &&...args)
        : msg_(FormatSafe(fmt, std::forward<Args>(args)...)) {}

    const char *what(void) const noexcept override { return msg_.c_str(); }

private:
    template <typename... Args>
    static std::string FormatSafe(fmt::format_string<Args...> fmt, Args &&...args) noexcept {
        try {
            return fmt::format(fmt, std::forward<Args>(args)...);
        } catch (...) {
            return std::string("FmtException (message formatting failed)");
        }
    }

    // Private: `what()` returns a pointer into this string, so nothing may reassign it.
    std::string msg_;
};
