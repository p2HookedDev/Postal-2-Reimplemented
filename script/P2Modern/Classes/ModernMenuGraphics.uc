// The main Modern Graphics page: the master box, the preset and the three pages of rows. Choosing Low to Ultra writes
// every level key of that preset; Custom keeps them.
class ModernMenuGraphics extends ModernMenuPage;

var UWindowCheckbox         MasterBox;
var UWindowComboControl     PresetCombo;
var ShellMenuChoice         LightingChoice;
var ShellMenuChoice         ImageChoice;
var ShellMenuChoice         EffectsChoice;
var localized string        CustomName;

var string                  PresetKeys[9];
// Preset P sets PresetKeys[K] to PresetValues[P * 9 + K].
var int                     PresetValues[36];

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	MasterBox = AddCheckbox(Labels[0], Helps[0], ItemFont);
	PresetCombo = AddLevelCombo(1, "ModernPreset", 2, 4);
	AddNames(PresetCombo, 1, 4);
	PresetCombo.AddItem(CustomName);
	LightingChoice = AddChoice(Labels[2], Helps[2], ItemFont, ItemAlign);
	ImageChoice = AddChoice(Labels[3], Helps[3], ItemFont, ItemAlign);
	EffectsChoice = AddChoice(Labels[4], Helps[4], ItemFont, ItemAlign);
	AddBack();
	LoadRows();
}

function LoadRows()
{
	bUpdate = false;
	MasterBox.SetValue(MasterOn());
	Super.LoadRows();
}

function RowChanged(int i)
{
	local int Preset;
	local int k;

	Preset = PresetCombo.GetSelectedIndex();
	if (Preset < 0 || Preset * ArrayCount(PresetKeys) >= ArrayCount(PresetValues))
		return;
	for (k = 0; k < ArrayCount(PresetKeys); k++)
		WriteKey(PresetKeys[k], string(PresetValues[Preset * ArrayCount(PresetKeys) + k]));
}

function Notify(UWindowDialogControl C, byte E)
{
	Super.Notify(C, E);

	if (C == None)
		return;
	if (E == DE_Click && C == LightingChoice)
		GoToMenu(class'ModernMenuLighting');
	else if (E == DE_Click && C == ImageChoice)
		GoToMenu(class'ModernMenuImage');
	else if (E == DE_Click && C == EffectsChoice)
		GoToMenu(class'ModernMenuEffects');
	else if (E == DE_Change && bUpdate && C == MasterBox)
	{
		WriteKey("ModernGraphics", string(MasterBox.bChecked));
		LoadRows();
	}
}

defaultproperties
{
	TitleText="Modern Graphics"
	BackHelp="Return to the Options menu."
	CustomName="Custom"

	Labels(0)="Modern graphics"
	Labels(1)="Quality preset"
	Labels(2)="Shadows and lighting"
	Labels(3)="Image and effects"
	Labels(4)="Picture effects"

	Helps(0)="Turns all the new graphics on or off. Off gives the original look."
	Helps(1)="Sets every quality row at once. Changing a row yourself makes it Custom."
	Helps(2)="Sun and lamp shadows, contact shadows, ambient occlusion, indirect light and rounded terrain."
	Helps(3)="Edge smoothing, bloom, sharpening, surface relief and generated specular."
	Helps(4)="Sun rays, vignette, film grain, colour fringe, colour strength, contrast and exposure."

	PresetKeys(0)="ModernSunShadows"
	PresetKeys(1)="ModernShadowDistance"
	PresetKeys(2)="ModernLampShadows"
	PresetKeys(3)="ModernContactShadows"
	PresetKeys(4)="ModernAO"
	PresetKeys(5)="ModernIndirect"
	PresetKeys(6)="ModernAntiAliasing"
	PresetKeys(7)="ModernBloom"
	PresetKeys(8)="ModernSharpen"

	PresetValues(0)=1
	PresetValues(1)=2
	PresetValues(2)=1
	PresetValues(3)=0
	PresetValues(4)=0
	PresetValues(5)=1
	PresetValues(6)=1
	PresetValues(7)=4
	PresetValues(8)=3

	PresetValues(9)=2
	PresetValues(10)=3
	PresetValues(11)=2
	PresetValues(12)=1
	PresetValues(13)=0
	PresetValues(14)=1
	PresetValues(15)=2
	PresetValues(16)=5
	PresetValues(17)=4

	PresetValues(18)=3
	PresetValues(19)=4
	PresetValues(20)=3
	PresetValues(21)=1
	PresetValues(22)=2
	PresetValues(23)=2
	PresetValues(24)=2
	PresetValues(25)=5
	PresetValues(26)=4

	PresetValues(27)=4
	PresetValues(28)=5
	PresetValues(29)=4
	PresetValues(30)=2
	PresetValues(31)=3
	PresetValues(32)=2
	PresetValues(33)=2
	PresetValues(34)=5
	PresetValues(35)=4
}
