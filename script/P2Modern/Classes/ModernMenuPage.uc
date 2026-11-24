// Shared by the Modern Graphics pages. Each row is one render device key, read with `get` and written with
// `set ini:Engine.Engine.RenderDevice`. Combos and sliders store numbers, boxes True or False; an empty key reads as
// the High preset's value. While the master is off every row is grey, and a change is undone.
class ModernMenuPage extends ShellMenuCW;

const KeyPath = "ini:Engine.Engine.RenderDevice";
const MasterKey = "ModernGraphics";
const PresetKey = "ModernPreset";
const CustomPreset = 4;

var UWindowDialogControl    Rows[8];
var string                  RowKeys[8];
var int                     RowHigh[8];
var int                     RowTop[8];
var int                     RowCount;

var localized string        Labels[8];
var localized string        Helps[8];
var localized string        LevelNames[5];
var localized string        TitleText;
var localized string        BackHelp;
var Color                   DimTextColor;
var bool                    bUpdate;

function string ReadKey(string Key)
{
	return GetPlayerOwner().ConsoleCommand("get" @ KeyPath @ Key);
}

function WriteKey(string Key, string Value)
{
	GetPlayerOwner().ConsoleCommand("set" @ KeyPath @ Key @ Value);
}

function bool MasterOn()
{
	local string Value;

	Value = ReadKey(MasterKey);
	return Value ~= "True" || Value == "1";
}

// The older True or False texts of a key that was a box read as the High value or 0.
function int ReadLevel(string Key, int High)
{
	local string Value;

	Value = ReadKey(Key);
	if (Value == "" || Value ~= "True")
		return High;
	if (Value ~= "False")
		return 0;
	return int(Value);
}

function AddRow(UWindowDialogControl C, string Key, int High, int Top)
{
	Rows[RowCount] = C;
	RowKeys[RowCount] = Key;
	RowHigh[RowCount] = High;
	RowTop[RowCount] = Top;
	RowCount++;
}

function UWindowComboControl AddLevelCombo(int Row, string Key, int High, int Top)
{
	local UWindowComboControl Combo;

	Combo = AddComboBox(Labels[Row], Helps[Row], ItemFont);
	AddRow(Combo, Key, High, Top);
	return Combo;
}

function AddNames(UWindowComboControl Combo, int First, int Last)
{
	local int i;

	for (i = First; i <= Last; i++)
		Combo.AddItem(LevelNames[i]);
}

function AddLevelSlider(int Row, string Key, int Low, int Top, int High)
{
	AddRow(AddSlider(Labels[Row], Helps[Row], ItemFont, Low, Top), Key, High, Top);
}

function AddBox(int Row, string Key)
{
	AddRow(AddCheckbox(Labels[Row], Helps[Row], ItemFont), Key, 1, 1);
}

function AddBack()
{
	BackChoice = AddChoice(BackText, BackHelp, ItemFont, ItemAlign, true);
}

function ShowRow(UWindowDialogControl C, bool bOn)
{
	local UWindowComboControl Combo;

	if (bOn)
		C.SetTextColor(ShellLookAndFeel(LookAndFeel).NormalTextColor);
	else
		C.SetTextColor(DimTextColor);
	if (UWindowCheckbox(C) != None)
		UWindowCheckbox(C).bDisabled = !bOn;
	Combo = UWindowComboControl(C);
	if (Combo != None && Combo.Button != None)
		Combo.Button.bDisabled = !bOn;
	if (Combo != None && Combo.LeftButton != None)
		Combo.LeftButton.bDisabled = !bOn;
	if (Combo != None && Combo.RightButton != None)
		Combo.RightButton.bDisabled = !bOn;
}

function LoadRows()
{
	local int i;
	local int Level;
	local bool bOn;

	bUpdate = false;
	bOn = MasterOn();
	for (i = 0; i < RowCount; i++)
	{
		Level = Clamp(ReadLevel(RowKeys[i], RowHigh[i]), 0, RowTop[i]);
		if (UWindowCheckbox(Rows[i]) != None)
			UWindowCheckbox(Rows[i]).SetValue(Level > 0);
		else if (UWindowComboControl(Rows[i]) != None)
			UWindowComboControl(Rows[i]).SetSelectedIndex(Level);
		else if (UWindowHSliderControl(Rows[i]) != None)
			UWindowHSliderControl(Rows[i]).SetValue(Level, true);
		ShowRow(Rows[i], bOn);
	}
	bUpdate = true;
}

function WriteRow(int i)
{
	if (UWindowCheckbox(Rows[i]) != None)
		WriteKey(RowKeys[i], string(UWindowCheckbox(Rows[i]).bChecked));
	else if (UWindowComboControl(Rows[i]) != None)
		WriteKey(RowKeys[i], string(UWindowComboControl(Rows[i]).GetSelectedIndex()));
	else if (UWindowHSliderControl(Rows[i]) != None)
		WriteKey(RowKeys[i], string(int(UWindowHSliderControl(Rows[i]).GetValue())));
}

// A row changed by hand makes the preset Custom; the main page's preset row overrides this.
function RowChanged(int i)
{
	WriteKey(PresetKey, string(CustomPreset));
}

function Notify(UWindowDialogControl C, byte E)
{
	local int i;

	Super.Notify(C, E);

	if (C == None)
		return;
	if (E == DE_Click && C == BackChoice)
	{
		GoBack();
		return;
	}
	if (E != DE_Change || !bUpdate)
		return;
	for (i = 0; i < RowCount; i++)
		if (C == Rows[i])
		{
			if (!MasterOn())
			{
				LoadRows();
				return;
			}
			WriteRow(i);
			RowChanged(i);
			return;
		}
}

defaultproperties
{
	LevelNames(0)="Off"
	LevelNames(1)="Low"
	LevelNames(2)="Medium"
	LevelNames(3)="High"
	LevelNames(4)="Ultra"

	DimTextColor=(R=110,G=110,B=110,A=255)

	MenuWidth=600.000000
	ItemHeight=26.000000
	ItemSpacingY=2.000000
	HintLines=3
}
