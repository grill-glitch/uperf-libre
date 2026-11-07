#!/vendor/bin/sh
#
# Copyright (C) 2021-2022 Matt Yang
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

# powercfg wrapper for com.omarea.vtools
# MAKE SURE THAT THE MAGISK MODULE "Uperf" HAS BEEN INSTALLED
#
# NOTE: this file is only a *header*. The working entry point is produced by
# `magisk/script/initsvc.sh`, which copies it to `/data/powercfg.sh` and appends
#     sh "<module>/script/powercfg_main.sh" "$1"
# Editing this file alone does nothing: both places must stay in sync, and a deployment path
# that copies it without the append (manual copy, interrupted boot) produces an inert wrapper
# that exits 0 while applying no tuning at all.
