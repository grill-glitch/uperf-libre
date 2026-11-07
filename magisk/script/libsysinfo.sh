#!/system/bin/sh
#
# Copyright (C) 2021-2022 Matt Yang & yinwanxi
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distriywxuted on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

###############################
# Platform info functions
###############################

# $1:"4.14" return:string_in_version
match_linux_version() {
    # -F: the argument is a fixed string, not a regex (the dots in "4.14" matched any char);
    # no `cat` pipeline.
    grep -iF -- "$1" /proc/version
}

get_socid() {
    if [ -f /sys/devices/soc0/soc_id ]; then
        echo "$(cat /sys/devices/soc0/soc_id)"
    else
        echo "$(cat /sys/devices/system/soc/soc0/id)"
    fi
}

get_nr_core() {
    # Anchored and multi-digit: `grep cpu[0-9]` counted any line *containing* cpuN and missed
    # cpu10+.
    grep -cE '^cpu[0-9]+' /proc/stat
}

# $1:cpuid
get_maxfreq() {
    # Failure must be distinguishable: an empty string made every caller's `[ ... -gt ... ]`
    # error out and silently select the *else* (lower-tier SoC) branch.
    local f="/sys/devices/system/cpu/cpu$1/cpufreq/cpuinfo_max_freq"
    [ -r "$f" ] || return 1
    cat "$f"
}

get_socname() {
    # Missing node must not print a `cat` error into the caller's value.
    [ -r /sys/devices/soc0/machine ] || return 1
    tr '[A-Z]' '[a-z]' < /sys/devices/soc0/machine
}

# ⑩ 32-bit coverage. Three things here are arch-locked: bin/uperf (the daemon),
# bin/libsfanalysis_rs.so (loaded into surfaceflinger) and the statically linked aarch64
# bin/busybox every boot script depends on. Everything else is shell or dex and arch-neutral.
# On a device whose primary ABI is not arm64-v8a none can exec -- and the injection target
# would itself be 32-bit -- so the install is refused up front instead of appearing to
# succeed and failing silently at boot.
#
# `UPERF_FAKE_ABI` is a testing seam for exercising the refusal path on a 64-bit device, in
# the same spirit as `UPERF_FAKE_ROOT`; it is only honoured together with
# `UPERF_SELFTEST=1` so a leftover variable cannot bypass the guard on a real install.
module_abi() {
    if [ -n "$UPERF_FAKE_ABI" ]; then
        if [ "$UPERF_SELFTEST" = "1" ]; then
            echo "uperf: honouring UPERF_FAKE_ABI=$UPERF_FAKE_ABI (self-test)" >&2
            echo "$UPERF_FAKE_ABI"
            return
        fi
        echo "uperf: ignoring UPERF_FAKE_ABI (set UPERF_SELFTEST=1 to use it)" >&2
    fi
    getprop ro.product.cpu.abi
}

is_aarch64() {
    if [ "$(module_abi)" == "arm64-v8a" ]; then
        echo "true"
    else
        echo "false"
    fi
}

# 1 when this module's binaries can run here, 0 otherwise.
abi_supported() {
    if [ "$(module_abi)" == "arm64-v8a" ]; then
        echo 1
    else
        echo 0
    fi
}

# Print the reason and return non-zero when the ABI is unsupported. Callers abort on it.
require_aarch64() {
    if [ "$(abi_supported)" == "1" ]; then
        return 0
    fi
    echo "! unsupported ABI: $(module_abi)"
    echo "  uperf-libre ships arm64-v8a binaries only:"
    echo "    bin/uperf                 the daemon"
    echo "    bin/libsfanalysis_rs.so   loaded into surfaceflinger"
    echo "    bin/busybox/busybox       aarch64 static, used by the boot scripts"
    echo "  None of them can exec on a 32-bit-only device, and the injection target"
    echo "  would be 32-bit too, so the install is refused rather than left to fail"
    echo "  silently at boot. No 32-bit build is shipped (docs/records/m11-10-32bit.md)."
    return 1
}

is_eas() {
    if [ "$(grep sched /sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors)" != "" ]; then
        echo "true"
    else
        echo "false"
    fi
}

is_mtk() {
    if [ "$(getprop | grep ro.mtk)" != "" ]; then
        echo "true"
    else
        echo "false"
    fi
}

miui_check(){
  # Test the property directly. The old `sort -V` against the literal "90" only worked while
  # the value started with a letter (`V12.5.3`): a digit-leading `12.5.3` sorted *before* "90"
  # and a real MIUI device reported false.
  local ver
  ver="$(getprop ro.miui.ui.version.name 2>/dev/null)"
  if [ -n "$ver" ]; then
    echo "true"
  else
    echo "false"
  fi
}

_get_sm6150_type() {
    case "$(get_socid)" in
    365 | 366) echo "sdm730" ;;
    355 | 369) echo "sdm675" ;;
    esac
}

_get_sdm76x_type() {
    if [ "$(get_maxfreq 7)" -gt 2300000 ]; then
        echo "sdm765"
    else
        echo "sdm750"
    fi
}

_get_lahaina_type() {
    if [ "$(get_maxfreq 7)" -gt 2600000 ]; then
        echo "sdm888"
    else
        if [ "$(get_maxfreq 4)" -gt 2300000 ]; then
            echo "sdm778"
        else
            echo "sdm780"
        fi
    fi
}

_get_taro_type() {
    if [ "$(get_socname)" == cape ] || [ "$(get_socname)" == capep ] || [ "$(get_socname)" == ukee ] || [ "$(get_socname)" == ukeep ]; then
        echo "sdm8g1+"
    else
        if [ "$(get_socname)" == waipio ] || [ "$(get_socname)" == waipiop ]; then
            echo "sdm8g1"
        else
            echo "sdm7g1"
        fi
    fi
}

_get_pinekala_type() {
    if [ "$(get_maxfreq 7)" -gt 3000000 ]; then
        echo "sdm8g3"
        else
        echo "sdm7+g3"
    fi
}

_get_kalama_type() {
    if [ "$(get_maxfreq 7)" -gt 3300000 ]; then
        echo "sdm8cxg2"
        else
        echo "sdm8g2"
    fi
}

# $1:board_name
get_config_name() {
    # Normalise the subject the same way `get_socname` does. Without this, any caller that
    # passed the lower-cased `get_socname` made every mixed-case key (MSM8916, msm8952Pro,
    # Infinix-X6827, …) unreachable and the device showed up as `unsupported`.
    local board
    board="$(printf '%s' "$1" | tr '[A-Z]' '[a-z]')"
    case "$board" in
    "pineapple") echo "$(_get_pinekala_type)" ;;
    "kalama") echo "$(_get_kalama_type)" ;;
    "taro") echo "$(_get_taro_type)" ;;
    "lahaina") echo "$(_get_lahaina_type)" ;;
    "shima") echo "$(_get_lahaina_type)" ;;
    "yupik") echo "$(_get_lahaina_type)" ;;
    "sun") echo "sdm8g4" ;;
    "kona") echo "sdm865" ;;    # 865, 870
    "msmnile") echo "sdm855" ;; # 855, 860
    "sdm845") echo "sdm845" ;;
    "lito") echo "$(_get_sdm76x_type)" ;;
    "sm6150") echo "$(_get_sm6150_type)" ;;
    "holi") echo "sdm695" ;;
    "sdm710") echo "sdm710" ;;
    "msm8939") echo "sdm616" ;;
    "msm8916") echo "sdm616" ;;
    "msm8916") echo "sdm616" ;;
    "sky") echo "sdm4g2" ;;
    "parrot") echo "sdm4g2" ;;
    "msm8937") echo "sdm439" ;;
    "msm8953") echo "sdm625" ;;    # 625
    "msm8953pro") echo "sdm625" ;; # 626
    "msm8952pro") echo "sdm653" ;; #653
    "msm8952") echo "sdm653" ;; #652
    "msm8976sg") echo "sdm653" ;; #653
    "sdm660") echo "sdm660" ;;
    "sdm636") echo "sdm660" ;;
    "trinket") echo "sdm665" ;; # sdm665
    "bengal") echo "sdm680" ;;  # sdm662
    "msm8956") echo "sdm650" ;;
    "msm8998") echo "sdm835" ;;
    "msm8996") echo "sdm820" ;;
    "msm8909") echo "sdm210" ;;
    "msm8996pro") echo "sdm820" ;;
    "s5e9925") echo "e2200" ;;
    "exynos2100") echo "e2100" ;;
    "erd8835") echo "e1380" ;;
    "universal9925") echo "e2200" ;;
    "exynos1080") echo "e1080" ;;
    "exynos990") echo "e990" ;;
    "universal1080") echo "e1080" ;;
    "universal990") echo "e990" ;;
    "universal5420") echo "e5420" ;;
    "universal9825") echo "e9820" ;;
    "universal9820") echo "e9820" ;;
    "universal9810") echo "e9810" ;;
    "universal8895") echo "e8895" ;;
    "universal8890") echo "e8890" ;;
    "universal7420") echo "e7420" ;;
    "mt6580") echo "mt6580" ;;
    "mt6765") echo "mtp35" ;; # Helio P35(mt6765)/G35(mt6765g)/G37(mt6765h)
    "mt6768") echo "mtg80" ;; # Helio P65(mt6768)/G70(mt6769v)/G80(mt6769t)/G85(mt6769z)
    "mt6771") echo "mtp70" ;;
    "mt8175") echo "mt8173" ;;
    "mt6779") echo "mtp90" ;;
    "mt6789") echo "mtg99" ;;
    "mt6785") echo "mtg90t" ;;
    "infinix-x6827") echo "mtg96" ;;
    "mt6827") echo "mtg96" ;;
    "mt6795") echo "mtx10" ;;
    "mt6797") echo "mtx20" ;;
    "mt6833") echo "mtd720" ;;
    "mt6833p") echo "mtd720" ;; # Dimensity 810
    "mt6833v") echo "mtd720" ;; # Dimensity 810
    "mt6835") echo "mtd6000" ;;
    "mt6853") echo "mtd720" ;;
    "mt6873") echo "mtd820" ;;
    "mt6875") echo "mtd820" ;;
    "mt6877") echo "mtd920" ;;
    "mt6855") echo "mtd930" ;;
    "cancunf") echo "mtd930" ;;
    "mt6885") echo "mtd1000" ;;
    "mt6886") echo "mtd7000" ;;
    "mt6889") echo "mtd1000" ;;
    "mt6891") echo "mtd1100" ;;
    "mt6893") echo "mtd1200" ;;
    "mt6895") echo "mtd8100" ;;
    "mt6897") echo "mtd8300" ;;
    "mt6983") echo "mtd9000" ;;
    "mt6985") echo "mtd9200" ;;
    "mt6989") echo "mtd9300" ;;
    "mt6991") echo "mtd9400" ;;
    "gs101") echo "gs101" ;;
    "gs201") echo "gs201" ;;
    "cheetah") echo "gs201" ;;
    "husky") echo "gs301" ;;
    "zuma") echo "gs301" ;;
    "shiba") echo "gs301" ;;
    "comet") echo "gs401" ;;
    "PRL") echo "kirin65x" ;;
    "BLN") echo "kirin65x" ;;
    "hi6250") echo "kirin65x" ;;
    "kirin655") echo "kirin65x" ;;
    "kirin658") echo "kirin65x" ;;
    "kirin659") echo "kirin65x" ;;
    "MAR") echo "kirin710" ;;
    "INE") echo "kirin710" ;;
    "ANE") echo "kirin710" ;;
    "JSN") echo "kirin710" ;;
    "kirin710") echo "kirin710" ;;
    "JKM-AL20") echo "kirin710" ;;
    "STF") echo "kirin960" ;;
    "HWI") echo "kirin960" ;;
    "kirin960") echo "kirin960" ;;
    "hi3660") echo "kirin960" ;;
    "CMR") echo "kirin960" ;;
    "BKL") echo "kirin970" ;;
    "ALP") echo "kirin970" ;;
    "COR") echo "kirin970" ;;
    "kirin970") echo "kirin970" ;;
    "ELE") echo "kirin980" ;;
    "kirin980") echo "kirin980" ;;
    "kirin990") echo "kirin990" ;;
    "LIO-AL00") echo "kirin990" ;;
    "OXP-AN00") echo "kirin990" ;;
    "ANA-AN00") echo "kirin990" ;;
    "ELS-AN10") echo "kirin990" ;;
    "EBG-AN00") echo "kirin990" ;;
    "TAS-AN00") echo "kirin990" ;;
    "LIO-AN00P") echo "kirin990" ;;
    "OXF-AN00") echo "kirin990" ;;
    "OCE-AL50") echo "kirin990" ;;
    "SCMR-W09") echo "kirin990" ;;
    "TAH-AN00m") echo "kirin990" ;;
    "kirin9000") echo "kirin9000" ;;
    "JAD") echo "kirin9000" ;;
    "hi3650") echo "kirin955" ;;
    "FRD") echo "kirin955" ;;
    "EVA-TL00") echo "kirin955" ;;
    "kirin955") echo "kirin955" ;;
    "sp9863a") echo "sp9863" ;;
    "ums312") echo "t310" ;;
    "ums9230") echo "t606" ;;
    "ums512") echo "t618" ;;
    "ud710") echo "t710" ;;
    "ums9620") echo "t770" ;;
    "sp9832e") echo "sp9832" ;;
    "ums312_2h10") echo "t310" ;;
    "ums9230_1h10") echo "t606" ;;
    "ums512_1h10") echo "t618" ;;
    "ud710_7h10") echo "t710" ;;
    "ums9620_2h10") echo "t770" ;;
    "sp9832e_1h10") echo "sp9832" ;;
    "sp9863a_1h10") echo "sp9863" ;;
    *) echo "unsupported" ;;
    esac
}
